import assert from "node:assert/strict";
import test from "node:test";

import { testInternals } from "../index.js";

test("recovers a later assistant reply after an early run error", async () => {
  let latestText = "previous reply";
  const runtime = {
    subagent: {
      async getSessionMessages({ sessionKey }) {
        assert.equal(sessionKey, "channel:user");
        return {
          messages: [{ role: "assistant", id: latestText, content: latestText }],
        };
      },
      async waitForRun({ runId }) {
        assert.equal(runId, "run-1");
        latestText = "fallback reply";
        return { status: "error" };
      },
    },
  };

  const recovered = await testInternals.recoverReplyAfterRunError({
    runtime,
    runId: "run-1",
    sessionKey: "channel:user",
    baselineReply: { key: "previous reply", text: "previous reply" },
    initialResult: { status: "error" },
    errorRecoveryMs: 100,
    withGatewayClient: (work) => work(),
    pollDelayMs: 0,
  });

  assert.deepEqual(recovered, {
    replyText: "fallback reply",
    attachments: [],
  });
});

test("does not treat an unchanged assistant reply as recovered", async () => {
  const runtime = {
    subagent: {
      async getSessionMessages() {
        return {
          messages: [
            { role: "assistant", id: "same", content: "previous reply" },
          ],
        };
      },
      async waitForRun() {
        return { status: "error" };
      },
    },
  };

  const recovered = await testInternals.recoverReplyAfterRunError({
    runtime,
    runId: "run-1",
    sessionKey: "channel:user",
    baselineReply: { key: "same", text: "previous reply" },
    initialResult: { status: "error" },
    errorRecoveryMs: 1,
    withGatewayClient: (work) => work(),
    pollDelayMs: 0,
  });

  assert.equal(recovered, null);
});

test("recovers attachment-only replies after an early run error", async () => {
  const runtime = {
    subagent: {
      async getSessionMessages() {
        return {
          messages: [{ role: "assistant", id: "same", content: "" }],
        };
      },
      async waitForRun() {
        return {
          status: "error",
          attachments: [
            {
              name: "diagram.png",
              mimeType: "image/png",
              dataBase64: "aW1hZ2U=",
            },
          ],
        };
      },
    },
  };

  const recovered = await testInternals.recoverReplyAfterRunError({
    runtime,
    runId: "run-1",
    sessionKey: "channel:user",
    baselineReply: { key: "same", text: "" },
    initialResult: { status: "error" },
    errorRecoveryMs: 100,
    withGatewayClient: (work) => work(),
    pollDelayMs: 0,
  });

  assert.equal(recovered.replyText, "");
  assert.deepEqual(recovered.attachments, [
    {
      name: "diagram.png",
      mimeType: "image/png",
      dataBase64: "aW1hZ2U=",
    },
  ]);
});

test("recovers attachment-only replies when assistant reply lookup fails", async () => {
  const runtime = {
    subagent: {
      async getSessionMessages() {
        throw new Error("session lookup failed");
      },
      async waitForRun() {
        return { status: "error" };
      },
    },
  };

  const recovered = await testInternals.recoverReplyAfterRunError({
    runtime,
    runId: "run-1",
    sessionKey: "channel:user",
    baselineReply: null,
    runStartedAtMs: Date.now(),
    initialResult: {
      status: "error",
      attachments: [
        {
          name: "diagram.png",
          mimeType: "image/png",
          dataBase64: "aW1hZ2U=",
        },
      ],
    },
    errorRecoveryMs: 100,
    withGatewayClient: (work) => work(),
    pollDelayMs: 0,
  });

  assert.equal(recovered.replyText, "");
  assert.deepEqual(recovered.attachments, [
    {
      name: "diagram.png",
      mimeType: "image/png",
      dataBase64: "aW1hZ2U=",
    },
  ]);
});

test("baseline read failures are downgraded to no baseline", async () => {
  const warnings = [];
  const reply = await testInternals.safeReadLatestAssistantReply({
    runtime: {
      subagent: {
        async getSessionMessages() {
          throw new Error("transient read failure");
        },
      },
    },
    sessionKey: "channel:user",
    log: { warn: (message) => warnings.push(message) },
    withGatewayClient: (work) => work(),
  });

  assert.equal(reply, null);
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /could not read latest assistant reply/);
});

test("baseline read timeouts are downgraded to no baseline", async () => {
  const reply = await testInternals.safeReadLatestAssistantReply({
    runtime: {
      subagent: {
        async getSessionMessages() {
          await new Promise(() => {});
        },
      },
    },
    sessionKey: "channel:user",
    log: { warn: () => {} },
    withGatewayClient: (work) => work(),
    timeoutMs: 1,
  });

  assert.equal(reply, null);
});

test("does not recover stale text when baseline is unavailable and reply has no timestamp", () => {
  const recovered = testInternals.isRecoverableReply(
    { key: "old", text: "old reply" },
    null,
    [],
    Date.now(),
  );

  assert.equal(recovered, false);
});

test("does not recover text from before the run start marker", () => {
  const runStartedAtMs = Date.parse("2026-05-01T12:00:00Z");
  const recovered = testInternals.isRecoverableReply(
    {
      key: "new-id",
      text: "older concurrent reply",
      createdAtMs: Date.parse("2026-05-01T11:59:59Z"),
    },
    { key: "old-id", text: "previous reply" },
    [],
    runStartedAtMs,
  );

  assert.equal(recovered, false);
});

test("recovers new text after the run start marker", () => {
  const runStartedAtMs = Date.parse("2026-05-01T12:00:00Z");
  const recovered = testInternals.isRecoverableReply(
    {
      key: "new-id",
      text: "fallback reply",
      createdAtMs: Date.parse("2026-05-01T12:00:01Z"),
    },
    { key: "old-id", text: "previous reply" },
    [],
    runStartedAtMs,
  );

  assert.equal(recovered, true);
});

test("parses numeric and ISO timestamps for recovery correlation", () => {
  assert.equal(
    testInternals.parseTimestampMillis(1_777_633_200),
    1_777_633_200_000,
  );
  assert.equal(
    testInternals.parseTimestampMillis("2026-05-01T12:00:00Z"),
    Date.parse("2026-05-01T12:00:00Z"),
  );
});
