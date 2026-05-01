import { definePluginEntry } from "openclaw/plugin-sdk/plugin-entry";

const DEFAULT_CONFIG = {
  clashdEndpoint: process.env.CLASHD_ENDPOINT || "http://localhost:9001/evaluate",
  timeoutMs: Number.parseInt(process.env.CLASHD_TIMEOUT_MS || "500", 10),
  fallbackOnError: process.env.CLASHD_FALLBACK || "deny",
};

export default definePluginEntry({
  id: "calciforge-policy",
  name: "Calciforge Policy Enforcement",
  description: "Enforces tool call policies via the Calciforge clashd sidecar.",

  register(api) {
    const config = {
      ...DEFAULT_CONFIG,
      ...(api.pluginConfig ?? {}),
    };

    api.logger.info("[calciforge-policy] Initializing policy enforcement");
    api.logger.info(
      `[calciforge-policy] clashd endpoint: ${config.clashdEndpoint}`,
    );

    checkClashdHealth(config.clashdEndpoint).then((healthy) => {
      if (healthy) {
        api.logger.info("[calciforge-policy] clashd health check: OK");
      } else {
        api.logger.warn(
          "[calciforge-policy] clashd health check: FAILED - policy enforcement may not work",
        );
      }
    });

    api.on("before_tool_call", async (event, context) => {
      const toolName = event.toolName || context.toolName || "unknown";
      const args = event.params || {};
      const identity = context.agentId || context.sessionKey || "unknown";

      api.logger.debug(
        `[calciforge-policy] Evaluating: ${toolName} for ${identity}`,
      );

      try {
        const verdict = await evaluateWithClashd(config, toolName, args, identity);

        if (verdict.verdict === "deny") {
          api.logger.info(
            `[calciforge-policy] DENIED: ${toolName} - ${verdict.reason}`,
          );
          return {
            block: true,
            blockReason: `Policy denied: ${
              verdict.reason || "operation blocked by security policy"
            }`,
          };
        }

        if (verdict.verdict === "review") {
          api.logger.info(
            `[calciforge-policy] REVIEW REQUIRED: ${toolName} - ${verdict.reason}`,
          );
          return {
            requireApproval: {
              title: `Calciforge policy review: ${toolName}`,
              description: `Policy review required: ${
                verdict.reason || "custodian approval needed"
              }`,
              severity: "warning",
              timeoutMs: 300000,
              timeoutBehavior: "deny",
            },
          };
        }

        api.logger.debug(`[calciforge-policy] ALLOWED: ${toolName}`);
        return { block: false };
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : String(error);
        api.logger.error(
          `[calciforge-policy] Error contacting clashd: ${errorMsg}`,
        );

        if (config.fallbackOnError === "deny") {
          return {
            block: true,
            blockReason: "Policy enforcement unavailable; failing closed",
          };
        }

        api.logger.warn(
          "[calciforge-policy] Allowing tool call because fallbackOnError=allow",
        );
        return { block: false };
      }
    });
  },
});

async function evaluateWithClashd(config, toolName, args, identity) {
  const controller = new AbortController();
  const timeoutId = setTimeout(
    () => controller.abort(),
    Number.isFinite(config.timeoutMs) ? config.timeoutMs : 500,
  );

  try {
    const response = await fetch(config.clashdEndpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        tool: toolName,
        args,
        context: {
          agent_id: identity,
          timestamp: new Date().toISOString(),
        },
      }),
      signal: controller.signal,
    });

    if (!response.ok) {
      throw new Error(`clashd returned ${response.status}`);
    }

    return await response.json();
  } finally {
    clearTimeout(timeoutId);
  }
}

async function checkClashdHealth(evaluateEndpoint) {
  try {
    const healthUrl = evaluateEndpoint.replace(/\/evaluate\/?$/, "/health");
    const response = await fetch(healthUrl, { signal: AbortSignal.timeout(1000) });
    return response.ok;
  } catch {
    return false;
  }
}
