import { definePluginEntry } from "openclaw/plugin-sdk/plugin-entry";

interface ClashdResponse {
  verdict: "allow" | "deny" | "review";
  reason?: string;
}

interface PolicyConfig {
  clashdEndpoint: string;
  timeoutMs: number;
  fallbackOnError: "allow" | "deny";
}

const DEFAULT_CONFIG: PolicyConfig = {
  clashdEndpoint:
    process.env.CLASHD_ENDPOINT || "http://localhost:9001/evaluate",
  timeoutMs: parseInt(process.env.CLASHD_TIMEOUT_MS || "500", 10),
  fallbackOnError: (process.env.CLASHD_FALLBACK as "allow" | "deny") || "deny",
};

/**
 * Calciforge Policy Plugin
 *
 * Integrates with clashd policy sidecar to enforce approval requirements
 * on critical operations (config changes, destructive commands, etc.)
 *
 * Requirements:
 * - OpenClaw >= 2026.3.24-beta.2 (for before_tool_call hook with requireApproval)
 * - clashd running on localhost:9001 (or CLASHD_ENDPOINT env var)
 *
 * Hook semantics:
 * - block: true = stop execution, return error to LLM
 * - requireApproval: true = pause for human approval via /approve command
 * - block: false = continue with tool execution
 */
export default definePluginEntry({
  id: "calciforge-policy",
  name: "Calciforge Policy Enforcement",
  description:
    "Enforces tool call policies via clashd sidecar - requires OpenClaw >= 2026.3.24-beta.2",

  register(api) {
    const config: PolicyConfig = {
      ...DEFAULT_CONFIG,
      // Could load from plugin config store in future
    };

    api.logger.info("[calciforge-policy] Initializing policy enforcement");
    api.logger.info(
      `[calciforge-policy] clashd endpoint: ${config.clashdEndpoint}`,
    );

    // Check clashd health on startup
    checkClashdHealth(config.clashdEndpoint).then((healthy) => {
      if (healthy) {
        api.logger.info("[calciforge-policy] clashd health check: OK");
      } else {
        api.logger.warn(
          "[calciforge-policy] clashd health check: FAILED - policy enforcement may not work",
        );
      }
    });

    // Register the typed before_tool_call hook.
    api.on(
      "before_tool_call",
      async (event, context) => {
        const toolName = event.toolName || context.toolName || "unknown";
        const args = event.params || {};
        const identity = context.agentId || context.sessionKey || "unknown";

        api.logger.debug(
          `[calciforge-policy] Evaluating: ${toolName} for ${identity}`,
        );

        try {
          const verdict = await evaluateWithClashd(
            config,
            toolName,
            args,
            identity,
          );

          if (verdict.verdict === "deny") {
            api.logger.info(
              `[calciforge-policy] DENIED: ${toolName} - ${verdict.reason}`,
            );
            return {
              block: true,
              blockReason: `Policy denied: ${verdict.reason || "operation blocked by security policy"}`,
            };
          }

          if (verdict.verdict === "review") {
            api.logger.info(
              `[calciforge-policy] REVIEW REQUIRED: ${toolName} - ${verdict.reason}`,
            );
            return {
              requireApproval: {
                title: `Calciforge policy review: ${toolName}`,
                description: `Policy review required: ${verdict.reason || "custodian approval needed"}`,
                severity: "warning",
                timeoutMs: 300_000,
                timeoutBehavior: "deny",
              },
            };
          }

          // verdict === "allow"
          api.logger.debug(`[calciforge-policy] ALLOWED: ${toolName}`);
          return { block: false };
        } catch (error) {
          const errorMsg =
            error instanceof Error ? error.message : String(error);
          api.logger.error(
            `[calciforge-policy] Error contacting clashd: ${errorMsg}`,
          );

          // Fail-safe: configurable fallback
          if (config.fallbackOnError === "deny") {
            api.logger.warn(
              `[calciforge-policy] Falling back to DENY due to clashd error`,
            );
            return {
              block: true,
              blockReason: `Policy enforcement unavailable: ${errorMsg}. Falling back to deny for safety.`,
            };
          } else {
            api.logger.warn(
              `[calciforge-policy] Falling back to ALLOW due to clashd error`,
            );
            return { block: false };
          }
        }
      },
    );

    api.logger.info("[calciforge-policy] Hook registered successfully");
  },
});

async function evaluateWithClashd(
  config: PolicyConfig,
  toolName: string,
  args: Record<string, unknown>,
  identity: string,
): Promise<ClashdResponse> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), config.timeoutMs);

  try {
    const response = await fetch(config.clashdEndpoint, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        tool: toolName,
        args,
        context: {
          identity,
          timestamp: new Date().toISOString(),
        },
      }),
      signal: controller.signal,
    });

    clearTimeout(timeoutId);

    if (!response.ok) {
      throw new Error(
        `clashd returned ${response.status}: ${await response.text()}`,
      );
    }

    const result: ClashdResponse = await response.json();
    return result;
  } catch (error) {
    clearTimeout(timeoutId);
    throw error;
  }
}

async function checkClashdHealth(endpoint: string): Promise<boolean> {
  try {
    const healthUrl = endpoint.replace("/evaluate", "/health");
    const response = await fetch(healthUrl, {
      method: "GET",
      signal: AbortSignal.timeout(2000),
    });
    return response.ok && (await response.text()) === "OK";
  } catch {
    return false;
  }
}
