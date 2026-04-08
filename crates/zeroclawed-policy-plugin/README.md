# ZeroClawed Policy Plugin for OpenClaw

Policy enforcement plugin that integrates with clashd sidecar to block destructive commands and require custodian approval for critical operations.

## Requirements

**Minimum OpenClaw Version:** `>=2026.3.24-beta.2`

**Required Features:**
- `before_tool_call` hook
- `requireApproval` in hook results (for custodian review flow)
- Plugin SDK with `definePluginEntry`

**Why this version?** The `requireApproval` hook result was added in 2026.3.24-beta.2. Earlier versions only support `block: true/false`, not the approval flow needed for custodian review.

## Installation

### 1. Ensure clashd is running

```bash
# Via Docker
docker run -d --name clashd -p 9001:9001 zeroclawed/clashd:latest

# Or via cargo (from zeroclawed repo)
cargo run -p clashd
```

Verify: `curl http://localhost:9001/health` should return `OK`

### 2. Install the plugin

```bash
# From ClawHub (when published)
openclaw plugins install clawhub:@zeroclawed/policy-plugin

# Or from local build
openclaw plugins install /path/to/zeroclawed/crates/zeroclawed-policy-plugin
```

### 3. Configure (optional)

Add to your `openclaw.json`:

```json
{
  "plugins": {
    "zeroclawed-policy": {
      "enabled": true
    }
  }
}
```

Or use environment variables:
- `CLASHD_ENDPOINT` - clashd URL (default: `http://localhost:9001/evaluate`)
- `CLASHD_TIMEOUT_MS` - request timeout (default: `500`)
- `CLASHD_FALLBACK` - `allow` or `deny` on clashd error (default: `deny`)

## How It Works

1. Every tool call triggers `before_tool_call` hook
2. Hook POSTs to clashd: `{ tool, args, context }`
3. clashd evaluates against policy and returns:
   - `allow` → tool executes normally
   - `deny` → blocked with error to LLM
   - `review` → paused for human approval

4. For `review`, OpenClaw shows approval UI and waits for:
   - `/approve <id>` command
   - Approval via Telegram/Discord buttons
   - Approval via gateway dashboard

## Protected Operations

By default, clashd requires review for:
- `gateway` `config.*` - Any OpenClaw config change
- `gateway` `restart` - Gateway restart
- `cron` `remove` - Removing cron jobs
- `write` to `.openclaw/` - Writing to OpenClaw config
- `edit` of `.openclaw/` - Editing OpenClaw config

And blocks:
- Destructive shell: `rm -rf`, `mkfs`, `wipefs`, `dd if=/dev/`

## Version Check During Installation

The plugin checks OpenClaw version on load and logs a warning if requirements aren't met:

```
[zeroclawed-policy] WARNING: OpenClaw version 2026.3.20 detected.
[zeroclawed-policy] This plugin requires >=2026.3.24-beta.2 for requireApproval support.
[zeroclawed-policy] Policy enforcement will not work correctly.
```

## Testing

```bash
# Test clashd directly
curl -X POST http://localhost:9001/evaluate \
  -H "Content-Type: application/json" \
  -d '{"tool":"gateway","args":{"action":"config.patch"}}'
# Expected: {"verdict":"review","reason":"..."}

# Test via OpenClaw
# Try: `gateway config.get` (should work)
# Try: `gateway config.patch ...` (should prompt for approval)
```

## Troubleshooting

| Issue | Solution |
|-------|----------|
| "clashd health check: FAILED" | Start clashd: `docker start clashd` or `cargo run -p clashd` |
| "Policy enforcement unavailable" | Check clashd is running and reachable at CLASHD_ENDPOINT |
| Gateway changes not blocked | Verify OpenClaw version >=2026.3.24-beta.2 |
| No approval prompt appearing | Check `before_tool_call` hooks are enabled in OpenClaw config |

## Building

```bash
cd crates/zeroclawed-policy-plugin
npm install
npm run build
```

## Integration with ZeroClawed

This plugin is part of the ZeroClawed project. For the full policy enforcement stack:

1. **clashd** (Rust) - Policy decision engine
2. **zeroclawed-policy-plugin** (this) - OpenClaw integration
3. **Clash** (Starlark) - Advanced policy rules (future)

See main ZeroClawed README for full architecture.
