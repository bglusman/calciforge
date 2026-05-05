import { createHash } from "node:crypto";

/**
 * Calciforge OpenClaw channel plugin.
 *
 * This registers POST /calciforge/inbound as a native OpenClaw channel route.
 * Calciforge sends inbound chat messages here, OpenClaw runs the selected
 * agent lane, and this plugin posts the assistant reply back to Calciforge's
 * /hooks/reply endpoint.
 */

async function getLegacyRegisterPluginHttpRoute() {
  const distDir = await resolveOpenClawDistDir();
  const mod = await import(`file://${distDir}/plugin-sdk/plugin-runtime.js`);
  return mod.registerPluginHttpRoute;
}

async function registerHttpRoute(api, route, log) {
  const registerLegacyRoute = await getLegacyRegisterPluginHttpRoute();
  const unregister = registerLegacyRoute({
    ...route,
    auth: "none",
    pluginId: "calciforge-channel",
    source: "calciforge-channel-plugin",
    replaceExisting: true,
    log: (msg) => log?.warn?.(msg),
  });
  return { unregister, source: "legacy route registry" };
}

let gatewayBoundRuntimePromise = null;
let gatewayScopeBridgePromise = null;

function getGatewayBoundRuntime() {
  gatewayBoundRuntimePromise ??= resolveOpenClawDistDir().then((distDir) =>
    import(`file://${distDir}/plugins/runtime/index.js`).then(
      ({ createPluginRuntime }) =>
        createPluginRuntime({ allowGatewaySubagentBinding: true }),
    ),
  );
  return gatewayBoundRuntimePromise;
}

async function resolveOpenClawDistDir() {
  const fs = await import("node:fs");
  for (const candidate of [
    "/usr/lib/node_modules/openclaw/dist",
    "/opt/homebrew/lib/node_modules/openclaw/dist",
    "/usr/local/lib/node_modules/openclaw/dist",
  ]) {
    if (fs.existsSync(candidate)) return candidate;
  }
  throw new Error("OpenClaw dist directory was not found");
}

function getGatewayScopeBridge() {
  gatewayScopeBridgePromise ??= import("node:fs").then(async (fs) => {
    const distDir = await resolveOpenClawDistDir();
    const files = fs.readdirSync(distDir);
    const bridgeFile =
      files.find((name) => /^gateway-request-scope-.*\.js$/.test(name)) ??
      files.find((name) => /^loader-.*\.js$/.test(name));
    if (!bridgeFile) {
      throw new Error("OpenClaw gateway request scope bridge was not found");
    }
    const mod = await import(`file://${distDir}/${bridgeFile}`);
    const withGatewayScope =
      mod.withPluginRuntimeGatewayRequestScope ?? mod.u ?? mod.n;
    if (typeof withGatewayScope !== "function") {
      throw new Error("OpenClaw gateway request scope bridge is incompatible");
    }
    return { withGatewayScope };
  });
  return gatewayScopeBridgePromise;
}

async function runWithSyntheticGatewayClient(work) {
  const { withGatewayScope } = await getGatewayScopeBridge();
  return withGatewayScope(
    {
      pluginId: "calciforge-channel",
      isWebchatConnect: () => false,
    },
    work,
  );
}

export default function register(api) {
  const pluginConfig = api.pluginConfig ?? {};
  const { authToken, replyWebhook, replyAuthToken } = pluginConfig;
  const runTimeoutMs = positiveInteger(pluginConfig.runTimeoutMs, 300000);
  const errorRecoveryMs = positiveInteger(pluginConfig.errorRecoveryMs, 120000);

  if (authToken && replyWebhook && replyAuthToken) {
    api.logger.info(
      `[calciforge-channel] plugin loaded - replyWebhook=${replyWebhook}`,
    );

    registerHttpRoute(api, {
      path: "/calciforge/inbound",
      match: "exact",
      handler: async (req, res) =>
        handleInboundRequest({
          getRuntime: getGatewayBoundRuntime,
          req,
          res,
          authToken,
          replyWebhook,
          replyAuthToken,
          runTimeoutMs,
          errorRecoveryMs,
          log: api.logger,
        }),
    }, api.logger)
      .then(({ source }) => {
        api.logger.info(
          `[calciforge-channel] registered POST /calciforge/inbound via ${source}`,
        );
      })
      .catch((err) => {
        api.logger.error(
          `[calciforge-channel] failed to register HTTP route: ${err.message}`,
        );
      });
  }
}

async function handleInboundRequest({
  getRuntime,
  req,
  res,
  authToken,
  replyWebhook,
  replyAuthToken,
  runTimeoutMs,
  errorRecoveryMs,
  log,
}) {
  if (req.method === "GET") {
    if (!isAuthorized(req, authToken)) {
      json(res, 401, { error: "Unauthorized" });
      return true;
    }
    json(res, 200, buildStatusPayload({ replyWebhook, replyAuthToken }));
    return true;
  }

  if (req.method !== "POST") {
    json(res, 405, { error: "Method not allowed" });
    return true;
  }

  if (!isAuthorized(req, authToken)) {
    json(res, 401, { error: "Unauthorized" });
    return true;
  }

  let body;
  try {
    body = await readJsonBody(req);
  } catch {
    json(res, 400, { error: "Invalid JSON body" });
    return true;
  }

  const { message, sessionKey, requestId, channel, replyTo, agentId } = body;
  if (!message || !sessionKey) {
    json(res, 400, { error: "message and sessionKey are required" });
    return true;
  }

  json(res, 200, { ok: true });

  try {
    const runtime = await getRuntime();
    if (canUseChannelRuntime(runtime)) {
      await dispatchViaChannelRuntime({
        runtime,
        message,
        sessionKey,
        requestId,
        channel,
        replyTo,
        agentId,
        sender: body.sender,
        replyWebhook,
        replyAuthToken,
        log,
      });
      return true;
    }

    log?.warn?.(
      "[calciforge-channel] OpenClaw channel runtime unavailable; falling back to subagent runtime",
    );
    await dispatchViaSubagentRuntime({
      runtime,
      message,
      sessionKey,
      requestId,
      channel,
      replyTo,
      agentId,
      replyWebhook,
      replyAuthToken,
      runTimeoutMs,
      errorRecoveryMs,
      log,
    });
  } catch (err) {
    log?.error?.(`[calciforge-channel] dispatch error - ${err.message}`);
    await deliverReply({
      replyWebhook,
      replyAuthToken,
      sessionKey,
      requestId,
      message: `OpenClaw dispatch failed: ${err.message}`,
      channel,
      replyTo,
      log,
    });
  }

  return true;
}

function canUseChannelRuntime(runtime) {
  return Boolean(
    runtime?.config?.current &&
      runtime?.channel?.turn?.run &&
      runtime?.channel?.session?.recordInboundSession &&
      runtime?.channel?.session?.resolveStorePath &&
      runtime?.channel?.session?.readSessionUpdatedAt &&
      runtime?.channel?.reply?.dispatchReplyFromConfig &&
      runtime?.channel?.reply?.finalizeInboundContext &&
      runtime?.channel?.reply?.formatInboundEnvelope &&
      runtime?.channel?.reply?.resolveEnvelopeFormatOptions &&
      runtime?.channel?.reply?.withReplyDispatcher,
  );
}

async function dispatchViaSubagentRuntime({
  runtime,
  message,
  sessionKey,
  requestId,
  channel,
  replyTo,
  agentId,
  replyWebhook,
  replyAuthToken,
  runTimeoutMs,
  errorRecoveryMs,
  log,
}) {
    const baselineReply = await safeReadLatestAssistantReply({
      runtime,
      sessionKey,
      log,
      timeoutMs: 5000,
    });
    const runStartedAtMs = Date.now();
    const { runId, result } = await runWithSyntheticGatewayClient(async () => {
      const { runId } = await runtime.subagent.run({
        sessionKey,
        message,
        idempotencyKey: `calciforge:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`,
        ...(agentId ? { lane: agentId } : {}),
        deliver: false,
      });

      const result = await runtime.subagent.waitForRun({
        runId,
        timeoutMs: runTimeoutMs,
      });
      return { runId, result };
    });

    if (result.status !== "ok") {
      log?.warn?.(
        `[calciforge-channel] agent run ${result.status} - runId=${runId}`,
      );
      const recovered = await recoverReplyAfterRunError({
        runtime,
        runId,
        sessionKey,
        baselineReply,
        runStartedAtMs,
        initialResult: result,
        errorRecoveryMs,
        log,
      });
      if (recovered) {
        await deliverReply({
          replyWebhook,
          replyAuthToken,
          sessionKey,
          requestId,
          message: recovered.replyText,
          attachments: recovered.attachments,
          channel,
          replyTo,
          log,
        });
        return true;
      }

      await deliverReply({
        replyWebhook,
        replyAuthToken,
        sessionKey,
        requestId,
        message: `OpenClaw run ${result.status}`,
        channel,
        replyTo,
        log,
      });
      return true;
    }

    const reply = await runWithSyntheticGatewayClient(() =>
      readLatestAssistantReply(runtime, sessionKey),
    );
    const attachments = normalizeAttachments(result.attachments);
    if (isSilentReply(reply.text) && attachments.length === 0) {
      log?.warn?.("[calciforge-channel] silent reply - reporting no visible reply");
      await deliverNoVisibleReply({
        replyWebhook,
        replyAuthToken,
        sessionKey,
        requestId,
        channel,
        replyTo,
        log,
      });
      return true;
    }

    await deliverReply({
      replyWebhook,
      replyAuthToken,
      sessionKey,
      requestId,
      message: reply.text,
      attachments,
      channel,
      replyTo,
      log,
    });
}

async function dispatchViaChannelRuntime({
  runtime,
  message,
  sessionKey,
  requestId,
  channel,
  replyTo,
  agentId,
  sender,
  replyWebhook,
  replyAuthToken,
  log,
}) {
  const cfg = runtime.config.current();
  const resolvedAgentId = agentId || parseAgentIdFromSessionKey(sessionKey) || "main";
  const accountId = "default";
  const sourceChannel = normalizeChannelName(channel);
  const senderId = normalizeString(sender) || normalizeString(replyTo) || sessionKey;
  const timestamp = Date.now();
  const storePath = runtime.channel.session.resolveStorePath(cfg.session?.store, {
    agentId: resolvedAgentId,
  });
  const ctxPayload = buildCalciforgeChannelContext({
    runtime,
    cfg,
    message,
    sessionKey,
    sourceChannel,
    senderId,
    accountId,
    agentId: resolvedAgentId,
    requestId,
    timestamp,
  });
  const dispatcher = createSingleReplyDispatcher();

  await runtime.channel.turn.run({
    channel: "calciforge",
    accountId,
    raw: {
      message,
      sessionKey,
      requestId,
      channel: sourceChannel,
      sender: senderId,
    },
    adapter: {
      ingest: () => ({
        id: requestId || `calciforge:${timestamp}`,
        timestamp,
        rawText: message,
        textForAgent: ctxPayload.BodyForAgent,
        textForCommands: ctxPayload.CommandBody,
        raw: message,
      }),
      resolveTurn: () => ({
        channel: "calciforge",
        accountId,
        routeSessionKey: sessionKey,
        storePath,
        ctxPayload,
        recordInboundSession: runtime.channel.session.recordInboundSession,
        record: {
          onRecordError: (err) =>
            log?.warn?.(
              `[calciforge-channel] failed to record inbound session: ${err.message}`,
            ),
        },
        runDispatch: () =>
          runtime.channel.reply.withReplyDispatcher({
            dispatcher,
            run: () =>
              runtime.channel.reply.dispatchReplyFromConfig({
                ctx: ctxPayload,
                cfg,
                dispatcher,
              }),
          }),
      }),
    },
  });

  const reply = dispatcher.takeReply();
  if (!reply || isSilentReply(reply.text)) {
    log?.warn?.(
      "[calciforge-channel] silent channel-runtime reply - reporting no visible reply",
    );
    await deliverNoVisibleReply({
      replyWebhook,
      replyAuthToken,
      sessionKey,
      requestId,
      channel: sourceChannel,
      replyTo,
      log,
    });
    return;
  }

  await deliverReply({
    replyWebhook,
    replyAuthToken,
    sessionKey,
    requestId,
    message: reply.text,
    attachments: normalizeAttachments(reply.attachments),
    channel: sourceChannel,
    replyTo,
    log,
  });
}

function buildCalciforgeChannelContext({
  runtime,
  cfg,
  message,
  sessionKey,
  sourceChannel,
  senderId,
  accountId,
  agentId,
  requestId,
  timestamp,
}) {
  const resolvedAgentId = agentId || parseAgentIdFromSessionKey(sessionKey) || "main";
  const previousTimestamp = runtime.channel.session.readSessionUpdatedAt({
    storePath: runtime.channel.session.resolveStorePath(cfg.session?.store, {
      agentId: resolvedAgentId,
    }),
    sessionKey,
  });
  const envelopeOptions = runtime.channel.reply.resolveEnvelopeFormatOptions(cfg);
  const body = runtime.channel.reply.formatInboundEnvelope({
    channel: "Calciforge",
    from: `${sourceChannel}:${senderId}`,
    timestamp,
    body: message,
    chatType: "direct",
    sender: {
      id: senderId,
    },
    previousTimestamp,
    envelope: envelopeOptions,
  });
  const isNativeCommand = message.trimStart().startsWith("/");

  return runtime.channel.reply.finalizeInboundContext({
    Body: body,
    BodyForAgent: message,
    RawBody: message,
    CommandBody: message,
    BodyForCommands: message,
    From: `${sourceChannel}:${senderId}`,
    To: `calciforge:${senderId}`,
    SessionKey: sessionKey,
    AccountId: accountId,
    ChatType: "direct",
    ConversationLabel: `${sourceChannel}:${senderId}`,
    SenderId: senderId,
    Provider: "calciforge",
    Surface: "calciforge",
    WasMentioned: true,
    CommandAuthorized: true,
    CommandSource: isNativeCommand ? "native" : "text",
    CommandTargetSessionKey: sessionKey,
    MessageSid: requestId,
    Timestamp: timestamp,
    NativeChannelId: sourceChannel,
    OriginatingChannel: "calciforge",
    OriginatingTo: `calciforge:${senderId}`,
  });
}

function createSingleReplyDispatcher() {
  const replies = [];
  const counts = { tool: 0, block: 0, final: 0 };
  const push = (kind, payload) => {
    counts[kind] += 1;
    replies.push({ kind, payload });
    return true;
  };

  return {
    sendToolResult: (payload) => push("tool", payload),
    sendBlockReply: (payload) => push("block", payload),
    sendFinalReply: (payload) => push("final", payload),
    waitForIdle: async () => {},
    markComplete: () => {},
    getQueuedCounts: () => ({ ...counts }),
    getFailedCounts: () => ({ tool: 0, block: 0, final: 0 }),
    takeReply: () => {
      const selected =
        [...replies].reverse().find((entry) => entry.kind === "final") ??
        [...replies].reverse().find((entry) => entry.kind === "block") ??
        [...replies].reverse().find((entry) => entry.kind === "tool");
      if (!selected) return null;
      return normalizeReplyPayloadForCalciforge(selected.payload);
    },
  };
}

function normalizeReplyPayloadForCalciforge(payload) {
  if (typeof payload === "string") return { text: payload, attachments: [] };
  if (!payload || typeof payload !== "object") return { text: "", attachments: [] };
  return {
    text: typeof payload.text === "string" ? payload.text : "",
    attachments: payload.attachments ?? payload.media ?? [],
  };
}

function normalizeChannelName(value) {
  return normalizeString(value) || "calciforge";
}

function normalizeString(value) {
  return typeof value === "string" ? value.trim() : "";
}

function parseAgentIdFromSessionKey(sessionKey) {
  const match = /^calciforge:([^:]+):/.exec(sessionKey);
  return match?.[1] || null;
}

function isAuthorized(req, expectedToken) {
  if (!expectedToken) return false;
  const authHeader = req.headers["authorization"] ?? "";
  const token = authHeader.startsWith("Bearer ")
    ? authHeader.slice("Bearer ".length)
    : authHeader;
  return token === expectedToken;
}

function buildStatusPayload({ replyWebhook, replyAuthToken }) {
  return {
    ok: true,
    plugin: "calciforge-channel",
    replyWebhook,
    replyAuthTokenSha256: sha256Hex(replyAuthToken).slice(0, 16),
  };
}

function sha256Hex(value) {
  return createHash("sha256").update(String(value ?? ""), "utf8").digest("hex");
}

async function readJsonBody(req) {
  const chunks = [];
  await new Promise((resolve, reject) => {
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", resolve);
    req.on("error", reject);
  });
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function isSilentReply(replyText) {
  const trimmed = (replyText ?? "").trim();
  return !trimmed || trimmed === "NO_REPLY" || trimmed === "HEARTBEAT_OK";
}

async function recoverReplyAfterRunError({
  runtime,
  runId,
  sessionKey,
  baselineReply,
  runStartedAtMs,
  initialResult,
  errorRecoveryMs,
  log,
  withGatewayClient = runWithSyntheticGatewayClient,
  pollDelayMs = 1000,
}) {
  const deadline = Date.now() + errorRecoveryMs;
  let lastResult = initialResult;

  while (Date.now() < deadline) {
    const remainingMs = deadline - Date.now();
    if (remainingMs <= 0) break;
    const reply = await safeReadLatestAssistantReply({
      runtime,
      sessionKey,
      log,
      withGatewayClient,
      timeoutMs: Math.min(5000, remainingMs),
    });
    const attachments = normalizeAttachments(lastResult?.attachments);
    if (
      isRecoverableReply(reply, baselineReply, attachments, runStartedAtMs) &&
      (!isSilentReply(reply?.text) || attachments.length > 0)
    ) {
      log?.info?.(
        `[calciforge-channel] recovered reply after run ${lastResult?.status ?? "error"} - runId=${runId}`,
      );
      return { replyText: reply?.text ?? "", attachments };
    }

    try {
      lastResult = await withGatewayClient(() =>
        runtime.subagent.waitForRun({
          runId,
          timeoutMs: Math.min(15000, remainingMs),
        }),
      );
      const waitedReply = await safeReadLatestAssistantReply({
        runtime,
        sessionKey,
        log,
        withGatewayClient,
        timeoutMs: Math.min(5000, Math.max(1, deadline - Date.now())),
      });
      const waitedAttachments = normalizeAttachments(lastResult.attachments);
      if (
        isRecoverableReply(
          waitedReply,
          baselineReply,
          waitedAttachments,
          runStartedAtMs,
        ) &&
        (!isSilentReply(waitedReply?.text) || waitedAttachments.length > 0)
      ) {
        return {
          replyText: waitedReply?.text ?? "",
          attachments: waitedAttachments,
        };
      }
    } catch (err) {
      log?.warn?.(
        `[calciforge-channel] error recovery wait failed - ${err.message}`,
      );
    }

    if (Date.now() >= deadline) break;
    await sleep(pollDelayMs);
  }

  return null;
}

function isNewReply(reply, baselineReply) {
  if (!reply?.text && !reply?.key) return false;
  if (!baselineReply) return true;
  return reply.key !== baselineReply.key || reply.text !== baselineReply.text;
}

function isRecoverableReply(reply, baselineReply, attachments, runStartedAtMs) {
  if (attachments.length > 0) return true;
  if (!isNewReply(reply, baselineReply)) return false;
  if (!baselineReply && !Number.isFinite(reply?.createdAtMs)) return false;
  return replyMatchesRunWindow(reply, runStartedAtMs);
}

function replyMatchesRunWindow(reply, runStartedAtMs) {
  if (!Number.isFinite(runStartedAtMs)) return true;
  if (!Number.isFinite(reply?.createdAtMs)) return false;
  return reply.createdAtMs >= runStartedAtMs;
}

async function safeReadLatestAssistantReply({
  runtime,
  sessionKey,
  log,
  withGatewayClient = runWithSyntheticGatewayClient,
  timeoutMs,
}) {
  try {
    return await withOptionalTimeout(
      withGatewayClient(() => readLatestAssistantReply(runtime, sessionKey)),
      timeoutMs,
    );
  } catch (err) {
    log?.warn?.(
      `[calciforge-channel] could not read latest assistant reply - ${err.message}`,
    );
    return null;
  }
}

async function readLatestAssistantReply(runtime, sessionKey) {
  const { messages } = await runtime.subagent.getSessionMessages({
    sessionKey,
    limit: 10,
  });
  const lastMsg = [...messages]
    .reverse()
    .find((msg) => msg?.role === "assistant");
  if (!lastMsg) return { key: null, text: "", createdAtMs: null };

  const content = lastMsg.content;
  const createdAtMs = parseTimestampMillis(
    lastMsg.createdAt ?? lastMsg.timestamp ?? lastMsg.created_at,
  );
  const key =
    lastMsg.id ??
    lastMsg.messageId ??
    lastMsg.createdAt ??
    lastMsg.timestamp ??
    JSON.stringify(content);
  if (typeof content === "string") return { key, text: content, createdAtMs };
  if (Array.isArray(content)) {
    return {
      key,
      text: content
        .filter((part) => part.type === "text")
        .map((part) => part.text ?? "")
        .join("\n"),
      createdAtMs,
    };
  }
  return { key, text: "", createdAtMs };
}

function parseTimestampMillis(value) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value < 1_000_000_000_000 ? value * 1000 : value;
  }
  if (typeof value !== "string" || value.trim() === "") return null;

  const numeric = Number(value);
  if (Number.isFinite(numeric)) {
    return numeric < 1_000_000_000_000 ? numeric * 1000 : numeric;
  }

  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

async function withOptionalTimeout(promise, timeoutMs) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) return promise;
  let timeoutId;
  const timeoutPromise = new Promise((_, reject) => {
    timeoutId = setTimeout(() => {
      reject(new Error(`timed out after ${timeoutMs}ms`));
    }, timeoutMs);
  });
  try {
    return await Promise.race([promise, timeoutPromise]);
  } finally {
    clearTimeout(timeoutId);
  }
}

async function deliverReply({
  replyWebhook,
  replyAuthToken,
  sessionKey,
  requestId,
  message,
  error,
  attachments,
  channel,
  replyTo,
  log,
}) {
  if (!replyWebhook || !replyAuthToken) {
    log?.warn?.("[calciforge-channel] reply webhook/auth not configured");
    return;
  }

  try {
    const payload = { sessionKey, message, error, channel, to: replyTo };
    if (requestId) {
      payload.requestId = requestId;
    }
    if (attachments?.length) {
      payload.attachments = attachments;
    }

    const resp = await fetch(replyWebhook, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${replyAuthToken}`,
      },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(30000),
    });

    if (!resp.ok) {
      log?.error?.(`[calciforge-channel] reply webhook failed - status=${resp.status}`);
    } else {
      log?.info?.("[calciforge-channel] reply delivered");
    }
  } catch (err) {
    log?.error?.(`[calciforge-channel] reply webhook error - ${err.message}`);
  }
}

async function deliverNoVisibleReply(args) {
  await deliverReply({
    ...args,
    error: "OpenClaw completed without a visible reply for this Calciforge request",
  });
}

function normalizeAttachments(value) {
  if (!Array.isArray(value)) return [];
  return value
    .map((attachment) => {
      if (!attachment || typeof attachment !== "object") return null;
      const dataBase64 = attachment.dataBase64 ?? attachment.data_base64;
      const mimeType = attachment.mimeType ?? attachment.mime_type;
      if (typeof dataBase64 !== "string" || !dataBase64) return null;
      if (mimeType !== undefined && typeof mimeType !== "string") return null;
      return {
        ...(typeof attachment.name === "string" ? { name: attachment.name } : {}),
        ...(typeof mimeType === "string" ? { mimeType } : {}),
        ...(typeof attachment.caption === "string" ? { caption: attachment.caption } : {}),
        dataBase64,
      };
    })
    .filter(Boolean);
}

function json(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}

function positiveInteger(value, fallback) {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const testInternals = {
  isNewReply,
  isRecoverableReply,
  parseTimestampMillis,
  safeReadLatestAssistantReply,
  withOptionalTimeout,
  recoverReplyAfterRunError,
  buildCalciforgeChannelContext,
  buildStatusPayload,
  createSingleReplyDispatcher,
  dispatchViaSubagentRuntime,
  dispatchViaChannelRuntime,
};
