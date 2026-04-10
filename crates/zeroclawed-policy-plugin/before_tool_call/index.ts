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
  requireApproval?: boolean;
  reason?: string;
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

  console.log(`[zeroclawed-policy] Evaluating: ${toolName} for ${agentId}`);

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
      return { block: true, reason: result.reason || "Policy denied" };
    }

    if (result.verdict === "review") {
      return { requireApproval: true, reason: result.reason || "Custodian approval required" };
    }

    return { block: false };

  } catch (error) {
    console.error(`[zeroclawed-policy] Error: ${error}`);
    // Fail-safe: deny on error
    return { block: true, reason: "Policy enforcement unavailable" };
  }
}
