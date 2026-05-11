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
declare const _default: {
    id: string;
    name: string;
    description: string;
    configSchema: import("openclaw/plugin-sdk/plugin-entry").OpenClawPluginConfigSchema;
    register: NonNullable<import("openclaw/plugin-sdk/plugin-entry").OpenClawPluginDefinition["register"]>;
} & Pick<import("openclaw/plugin-sdk/plugin-entry").OpenClawPluginDefinition, "kind" | "reload" | "nodeHostCommands" | "securityAuditCollectors">;
export default _default;
//# sourceMappingURL=index.d.ts.map