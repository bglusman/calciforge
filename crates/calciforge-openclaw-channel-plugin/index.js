/**
 * Calciforge OpenClaw channel plugin.
 *
 * This registers POST /calciforge/inbound as a native OpenClaw channel route.
 * Calciforge sends inbound chat messages here, OpenClaw runs the selected
 * agent lane, and this plugin posts the assistant reply back to Calciforge's
 * /hooks/reply endpoint.
 */

async function getLegacyRegisterPluginHttpRoute() {
  const mod = await import("/usr/lib/node_modules/openclaw/dist/http-registry-Cbhawt2w.js");
  return mod.t;
}

async function registerHttpRoute(api, route, log) {
  if (typeof api.registerHttpRoute === "function") {
    const unregister = api.registerHttpRoute({
      ...route,
      auth: "plugin",
      replaceExisting: true,
    });
    return {
      unregister: typeof unregister === "function" ? unregister : () => {},
      source: "api.registerHttpRoute",
    };
  }

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

export default function register(api) {
  const pluginConfig = api.pluginConfig ?? {};
  const { authToken, replyWebhook, replyAuthToken } = pluginConfig;

  api.logger.info(
    `[calciforge-channel] plugin loaded - replyWebhook=${replyWebhook ?? "(none)"}`,
  );

  api.registerChannel({
    plugin: {
      id: "calciforge-channel",
      name: "Calciforge",
      description: "Calciforge inbound channel",
      configSchema: { type: "object", properties: {}, additionalProperties: true },

      listAccounts: async () => [{ accountId: "default", config: {} }],

      resolveAccountSnapshot: ({ account }) => ({
        accountId: account.accountId,
        config: account.config,
        status: { kind: "connected", label: "Calciforge channel active" },
      }),

      send: null,

      gateway: {
        startAccount: async (ctx) => {
          const { log, signal } = ctx;

          let registration;
          try {
            registration = await registerHttpRoute(api, {
              path: "/calciforge/inbound",
              match: "exact",
              handler: async (req, res) =>
                handleInboundRequest({
                  api,
                  req,
                  res,
                  authToken,
                  replyWebhook,
                  replyAuthToken,
                  log,
                }),
            }, log);
          } catch (err) {
            log?.error?.(
              `[calciforge-channel] failed to register HTTP route: ${err.message}`,
            );
            return;
          }

          const { unregister, source } = registration;
          log?.info?.(
            `[calciforge-channel] registered POST /calciforge/inbound via ${source}`,
          );

          await new Promise((resolve) => {
            signal?.addEventListener("abort", () => {
              unregister();
              log?.info?.("[calciforge-channel] channel stopped");
              resolve();
            });
          });
        },
      },
    },
  });
}

async function handleInboundRequest({
  api,
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

  const { message, sessionKey, channel, replyTo, agentId } = body;
  if (!message || !sessionKey) {
    json(res, 400, { error: "message and sessionKey are required" });
    return true;
  }

  json(res, 200, { ok: true });

  try {
    const { runId } = await api.runtime.subagent.run({
      sessionKey,
      message,
      idempotencyKey: `calciforge:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`,
      ...(agentId ? { lane: agentId } : {}),
      deliver: false,
    });

    const result = await api.runtime.subagent.waitForRun({
      runId,
      timeoutMs: 300000,
    });

    if (result.status !== "ok") {
      log?.warn?.(
        `[calciforge-channel] agent run ${result.status} - runId=${runId}`,
      );
      await deliverReply({
        replyWebhook,
        replyAuthToken,
        sessionKey,
        message: `OpenClaw run ${result.status}`,
        channel,
        replyTo,
        log,
      });
      return true;
    }

    const replyText = await readLatestAssistantText(
      api,
      sessionKey,
    );
    if (isSilentReply(replyText)) {
      log?.info?.("[calciforge-channel] silent reply - not forwarding");
      return true;
    }

    await deliverReply({
      replyWebhook,
      replyAuthToken,
      sessionKey,
      message: replyText,
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

async function readLatestAssistantText(api, sessionKey) {
  const { messages } = await api.runtime.subagent.getSessionMessages({
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

function isSilentReply(replyText) {
  const trimmed = (replyText ?? "").trim();
  return !trimmed || trimmed === "NO_REPLY" || trimmed === "HEARTBEAT_OK";
}

async function deliverReply({
  replyWebhook,
  replyAuthToken,
  sessionKey,
  message,
  channel,
  replyTo,
  log,
}) {
  if (!replyWebhook || !replyAuthToken) {
    log?.warn?.("[calciforge-channel] reply webhook/auth not configured");
    return;
  }

  try {
    const resp = await fetch(replyWebhook, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${replyAuthToken}`,
      },
      body: JSON.stringify({ sessionKey, message, channel, to: replyTo }),
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

function json(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}
