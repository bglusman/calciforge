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
  log,
}) {
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
        timeoutMs: 300000,
      });
      return { runId, result };
    });

    if (result.status !== "ok") {
      log?.warn?.(
        `[calciforge-channel] agent run ${result.status} - runId=${runId}`,
      );
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

    const replyText = await runWithSyntheticGatewayClient(() =>
      readLatestAssistantText(runtime, sessionKey),
    );
    const attachments = normalizeAttachments(result.attachments);
    if (isSilentReply(replyText) && attachments.length === 0) {
      log?.info?.("[calciforge-channel] silent reply - not forwarding");
      return true;
    }

    await deliverReply({
      replyWebhook,
      replyAuthToken,
      sessionKey,
      requestId,
      message: replyText,
      attachments,
      channel,
      replyTo,
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

function isAuthorized(req, expectedToken) {
  if (!expectedToken) return false;
  const authHeader = req.headers["authorization"] ?? "";
  const token = authHeader.startsWith("Bearer ")
    ? authHeader.slice("Bearer ".length)
    : authHeader;
  return token === expectedToken;
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

async function readLatestAssistantText(runtime, sessionKey) {
  const { messages } = await runtime.subagent.getSessionMessages({
    sessionKey,
    limit: 10,
  });
  const lastMsg = [...messages]
    .reverse()
    .find((msg) => msg?.role === "assistant");
  if (!lastMsg) return "";

  const content = lastMsg.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .filter((part) => part.type === "text")
      .map((part) => part.text ?? "")
      .join("\n");
  }
  return "";
}

async function deliverReply({
  replyWebhook,
  replyAuthToken,
  sessionKey,
  requestId,
  message,
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
    const payload = { sessionKey, message, channel, to: replyTo };
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
