# zeroclawed-policy-plugin

OpenClaw plugin for policy enforcement via clashd sidecar.

## Overview

This plugin hooks into OpenClaw's `before_tool_call` lifecycle event to evaluate every tool execution against a centralized policy. It communicates with [clashd](../clashd/) — a Starlark-based policy sidecar.

## How It Works

```
OpenClaw Tool Call
        │
        ▼
┌──────────────────┐
│ before_tool_call │
│     (this)       │
└────────┬─────────┘
         │ POST /evaluate
         ▼
    ┌─────────┐
    │ clashd  │
    └────┬────┘
         │ JSON response
         ▼
    Allow / Block / Require Approval
```

## Installation

1. Ensure clashd is running (see [clashd README](../clashd/))
2. Install this plugin in OpenClaw's plugin directory:
   ```bash
   cp -r before_tool_call /path/to/openclaw/plugins/
   ```
3. Set environment variables (optional):
   ```bash
   export CLASHD_ENDPOINT="http://localhost:9001/evaluate"
   export CLASHD_TIMEOUT_MS="500"
   ```

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `CLASHD_ENDPOINT` | `http://localhost:9001/evaluate` | clashd evaluate endpoint |
| `CLASHD_TIMEOUT_MS` | `500` | Request timeout in milliseconds |

## Hook: before_tool_call

Invoked before every tool execution.

### Input

```typescript
interface HookContext {
  toolName: string;           // Tool being called
  args: Record<string, unknown>;  // Tool arguments
  session?: {
    identity?: string;        // Agent identity (e.g., "librarian")
  };
}
```

### Output

```typescript
interface HookResult {
  block?: boolean;           // true = deny the tool call
  requireApproval?: boolean; // true = custodian must approve
  reason?: string;           // Explanation for block/approval
}
```

### Behavior

| clashd Verdict | Plugin Result |
|----------------|---------------|
| `allow` | `{ block: false }` — tool executes normally |
| `deny` | `{ block: true, reason: "..." }` — tool blocked |
| `review` | `{ requireApproval: true, reason: "..." }` — custodian approval required |
| Error/Timeout | `{ block: true, reason: "Policy unavailable" }` — fail-closed |

## Example Flow

```
User: "Delete all files in /data"

OpenClaw → before_tool_call({toolName: "exec", args: {command: "rm -rf /data/*"}})

Plugin → POST clashd/evaluate
         {
           "tool": "exec",
           "args": {"command": "rm -rf /data/*"},
           "context": {"agent_id": "librarian"}
         }

clashd → Policy check:
         - Destructive pattern "rm -rf" detected
         → {"verdict": "review", "reason": "Destructive command requires approval"}

Plugin → {requireApproval: true, reason: "Destructive command requires approval"}

OpenClaw → Blocks execution, asks custodian for approval
```

## Development

```bash
# Install dependencies
npm install

# Type check
npx tsc --noEmit

# Test (manual)
node -e "
  const hook = require('./before_tool_call/index.ts');
  hook.default({toolName: 'test', args: {}}).then(console.log);
"
```

## See Also

- [clashd](../clashd/) — Policy sidecar documentation
- [OpenClaw Plugin System](../../docs/plugins.md) — General plugin docs
