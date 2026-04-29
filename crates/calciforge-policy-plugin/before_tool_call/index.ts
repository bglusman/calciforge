// before_tool_call hook handler
// This is invoked by OpenClaw before each tool execution

interface HookContext {
  toolName: string;
  args: Record<string, unknown>;
  session?: {
    identity?: string;
  };
}

interface HookResult {
  block?: boolean;
  blockReason?: string;
  requireApproval?: {
    title: string;
    description: string;
    severity?: "info" | "warning" | "critical";
    timeoutMs?: number;
    timeoutBehavior?: "allow" | "deny";
  };
}

interface ClashdResponse {
  verdict: "allow" | "deny" | "review";
  reason?: string;
}

const CLASHD_ENDPOINT = process.env.CLASHD_ENDPOINT || "http://localhost:9001/evaluate";
const CLASHD_TIMEOUT_MS = parseInt(process.env.CLASHD_TIMEOUT_MS || "500", 10);

export default async function beforeToolCall(context: HookContext): Promise<HookResult> {
  const toolName = context.toolName;
  const args = context.args;
  const agentId = context.session?.identity || "unknown";

  console.log(`[calciforge-policy] Evaluating: ${toolName} for ${agentId}`);

  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), CLASHD_TIMEOUT_MS);

    const response = await fetch(CLASHD_ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        tool: toolName,
        args,
        context: { agent_id: agentId, timestamp: new Date().toISOString() }
      }),
      signal: controller.signal
    });

    clearTimeout(timeoutId);

    if (!response.ok) {
      throw new Error(`clashd returned ${response.status}`);
    }

    const result: ClashdResponse = await response.json();

  if (result.verdict === "deny") {
      return { block: true, blockReason: result.reason || "Policy denied" };
    }

    if (result.verdict === "review") {
      return {
        requireApproval: {
          title: `Calciforge policy review: ${toolName}`,
          description: result.reason || "Custodian approval required",
          severity: "warning",
          timeoutMs: 300_000,
          timeoutBehavior: "deny",
        }
      };
    }

    return { block: false };

  } catch (error) {
    console.error(`[calciforge-policy] Error: ${error}`);
    // Fail-safe: deny on error
    return { block: true, blockReason: "Policy enforcement unavailable" };
  }
}
