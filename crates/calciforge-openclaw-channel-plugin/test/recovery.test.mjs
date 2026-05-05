import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import http from "node:http";
import test from "node:test";

import { testInternals } from "../index.js";

test("status payload reports callback URL and reply-token hash without exposing token", () => {
  const fixtureReplyToken = ["reply", "fixture"].join("-");
  const payload = testInternals.buildStatusPayload({
    replyWebhook: "http://198.51.100.10:18797/hooks/reply",
    replyAuthToken: fixtureReplyToken,
  });

  assert.deepEqual(payload, {
    ok: true,
    plugin: "calciforge-channel",
    replyWebhook: "http://198.51.100.10:18797/hooks/reply",
    replyAuthTokenSha256: createHash("sha256")
      .update(fixtureReplyToken, "utf8")
      .digest("hex")
      .slice(0, 16),
  });
  assert.equal(JSON.stringify(payload).includes(fixtureReplyToken), false);
});

test("dispatches through OpenClaw channel runtime with calciforge command context", async () => {
  const delivered = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      delivered.push({
        auth: req.headers.authorization,
        body: JSON.parse(Buffer.concat(chunks).toString("utf8")),
      });
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const replyWebhook = `http://127.0.0.1:${server.address().port}/reply`;
  const turnContexts = [];
  const runtime = {
    config: { current: () => ({ session: {} }) },
    channel: {
      session: {
        resolveStorePath: () => "/tmp/openclaw-test-sessions.json",
        readSessionUpdatedAt: () => undefined,
        recordInboundSession: async ({ ctx }) => {
          turnContexts.push(ctx);
        },
      },
      reply: {
        resolveEnvelopeFormatOptions: () => ({}),
        formatInboundEnvelope: ({ body }) => body,
        finalizeInboundContext: (ctx) => ctx,
        withReplyDispatcher: async ({ run }) => run(),
        dispatchReplyFromConfig: async ({ ctx, dispatcher }) => {
          assert.equal(ctx.Surface, "calciforge");
          assert.equal(ctx.Provider, "calciforge");
          assert.equal(ctx.NativeChannelId, "telegram");
          assert.equal(ctx.CommandAuthorized, true);
          assert.equal(ctx.CommandSource, "native");
          assert.equal(ctx.CommandTargetSessionKey, "calciforge:main:brian");
          dispatcher.sendFinalReply({ text: "Calciforge status: ok" });
          return { queuedFinal: true, counts: dispatcher.getQueuedCounts() };
        },
      },
      turn: {
        run: async ({ raw, adapter }) => {
          const input = adapter.ingest(raw);
          const resolved = adapter.resolveTurn(input);
          await resolved.recordInboundSession({
            storePath: resolved.storePath,
            sessionKey: resolved.routeSessionKey,
            ctx: resolved.ctxPayload,
          });
          return resolved.runDispatch();
        },
      },
    },
  };

  try {
    await testInternals.dispatchViaChannelRuntime({
      runtime,
      message: "/status",
      sessionKey: "calciforge:main:brian",
      requestId: "req-1",
      channel: "telegram",
      sender: "brian",
      replyWebhook,
      replyAuthToken: "reply-secret",
    });
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }

  assert.equal(turnContexts.length, 1);
  assert.equal(delivered.length, 1);
  assert.equal(delivered[0].auth, "Bearer reply-secret");
  assert.deepEqual(delivered[0].body, {
    sessionKey: "calciforge:main:brian",
    requestId: "req-1",
    message: "Calciforge status: ok",
    channel: "telegram",
  });
});

test("reports channel-runtime requests that complete without a visible reply", async () => {
  const delivered = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      delivered.push(JSON.parse(Buffer.concat(chunks).toString("utf8")));
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const replyWebhook = `http://127.0.0.1:${server.address().port}/reply`;
  const runtime = {
    config: { current: () => ({ session: {} }) },
    channel: {
      session: {
        resolveStorePath: () => "/tmp/openclaw-test-sessions.json",
        readSessionUpdatedAt: () => undefined,
        recordInboundSession: async () => {},
      },
      reply: {
        resolveEnvelopeFormatOptions: () => ({}),
        formatInboundEnvelope: ({ body }) => body,
        finalizeInboundContext: (ctx) => ctx,
        withReplyDispatcher: async ({ run }) => run(),
        dispatchReplyFromConfig: async ({ dispatcher }) => {
          assert.equal(dispatcher.getQueuedCounts().final, 0);
          return { queuedFinal: false, counts: dispatcher.getQueuedCounts() };
        },
      },
      turn: {
        run: async ({ raw, adapter }) => {
          const input = adapter.ingest(raw);
          const resolved = adapter.resolveTurn(input);
          return resolved.runDispatch();
        },
      },
    },
  };

  try {
    await testInternals.dispatchViaChannelRuntime({
      runtime,
      message: "hello?",
      sessionKey: "calciforge:main:brian",
      requestId: "silent-req",
      channel: "telegram",
      sender: "brian",
      replyWebhook,
      replyAuthToken: "reply-secret",
    });
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }

  assert.equal(delivered.length, 1);
  assert.deepEqual(delivered[0], {
    sessionKey: "calciforge:main:brian",
    requestId: "silent-req",
    error: "OpenClaw completed without a visible reply for this Calciforge request",
    channel: "telegram",
  });
});

test("reports subagent-runtime requests that complete without a visible reply", async () => {
  const delivered = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      delivered.push(JSON.parse(Buffer.concat(chunks).toString("utf8")));
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const replyWebhook = `http://127.0.0.1:${server.address().port}/reply`;
  const runtime = {
    subagent: {
      async getSessionMessages({ sessionKey }) {
        assert.equal(sessionKey, "calciforge:main:brian");
        return { messages: [{ role: "assistant", id: "silent", content: "" }] };
      },
      async run({ sessionKey, message, deliver }) {
        assert.equal(sessionKey, "calciforge:main:brian");
        assert.equal(message, "hello?");
        assert.equal(deliver, false);
        return { runId: "run-silent" };
      },
      async waitForRun({ runId }) {
        assert.equal(runId, "run-silent");
        return { status: "ok" };
      },
    },
  };

  try {
    await testInternals.dispatchViaSubagentRuntime({
      runtime,
      message: "hello?",
      sessionKey: "calciforge:main:brian",
      requestId: "silent-subagent-req",
      channel: "telegram",
      replyWebhook,
      replyAuthToken: "reply-secret",
      runTimeoutMs: 1000,
      errorRecoveryMs: 1,
    });
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }

  assert.equal(delivered.length, 1);
  assert.deepEqual(delivered[0], {
    sessionKey: "calciforge:main:brian",
    requestId: "silent-subagent-req",
    error: "OpenClaw completed without a visible reply for this Calciforge request",
    channel: "telegram",
  });
});

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

test("does not recover changed text without timestamp when baseline exists", () => {
  const recovered = testInternals.isRecoverableReply(
    {
      key: "other-run",
      text: "changed concurrent reply",
    },
    { key: "old-id", text: "previous reply" },
    [],
    Date.parse("2026-05-01T12:00:00Z"),
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
