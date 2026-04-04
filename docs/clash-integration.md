# Clash Policy Integration

## Overview

nonzeroclaw uses [clash](../crates/clash/) for policy enforcement —
Starlark-based rules evaluated before every tool execution.

Clash policies let operators express security rules as Python-like code:

- Allow safe/read-only operations freely
- Block destructive commands unconditionally
- Require human approval for risky-but-useful operations

Policy evaluation is **synchronous and fast** (~0.1–0.5 ms per call).
The Starlark VM is compiled once at startup; only evaluation is hot-path.

---

## Architecture

```
Tool Request (from LLM)
        │
        ▼
  Policy Evaluate
  (clash::ClashPolicy::evaluate)
        │
        ├─ Allow  ──────────────────────► Execute tool → return result
        │
        ├─ Deny   ──────────────────────► Return error to LLM (do NOT execute)
        │                                 Record clash_denies_total++
        │
        └─ Review ──────────────────────► If session cache hit → Execute
                                          Else → prompt_user()
                                             ├─ Approve  → Execute
                                             ├─ ApproveAlways → Cache + Execute
                                             └─ Deny → Return error to LLM
```

### Data flow in `run_tool_call_loop`

1. LLM emits a tool call (e.g. `shell` with `command = "rm -rf /tmp/build"`)
2. `run_tool_call_loop` builds a `PolicyContext` with:
   - `identity` = sender identity (e.g. `"owner"`, `"brian"`, `"guest"`)
   - `agent` = `"nonzeroclaw"`
   - `action` = `"tool:<tool_name>"` (e.g. `"tool:shell"`)
   - `extra["command"]` = command string (for shell calls)
   - `extra["path"]` = file path (for file_read / file_write / delete)
3. `policy.evaluate(&action, &ctx)` is called
4. The verdict gates execution:
   - `Allow` → proceed
   - `Deny(reason)` → inject `"Policy denied: {reason}"` into tool results
   - `Review(reason)` → check session cache, else call `prompt_user()`

---

## Policy Files

### Base policy

```
~/.nonzeroclaw/policy.star
```

Evaluated for every tool call. Ships with a default template in:
```
crates/nonzeroclaw/config/policy.star
```

Copy this template to `~/.nonzeroclaw/policy.star` and customize it.

### Per-identity profiles

```
~/.nonzeroclaw/profiles/{identity}.star
```

Override rules for specific users/agents. Profiles are loaded via
`StarlarkPolicy::load_with_profiles()`, which merges them with the base policy.

> **Important:** Profile overrides can only **add** restrictions.  
> A profile cannot loosen a base `Deny` to `Allow`.

---

## Policy API

```python
def evaluate(action, identity, agent, command="", path=""):
    """
    Return one of:
      "allow"            — proceed with execution
      "deny:<reason>"    — block; reason is surfaced to the LLM
      "review:<reason>"  — require human approval before proceeding
    """
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `action`  | `str` | Tool name as `tool:<name>` (e.g. `"tool:shell"`) |
| `identity`| `str` | Caller identity (e.g. `"owner"`, `"guest"`, channel user ID) |
| `agent`   | `str` | Agent instance identifier (always `"nonzeroclaw"` for NZC) |
| `command` | `str` | For shell/web_fetch: the command or URL being executed |
| `path`    | `str` | For file tools: the target filesystem path |

---

## Examples

### Block all shell for non-owners

```python
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and identity != "owner":
        return "deny:shell restricted to owner"
    return "allow"
```

### Require approval for file deletion

```python
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:delete":
        return "review:file_deletion: " + path[:60]
    return "allow"
```

### Block catastrophic commands, review destructive ones

```python
CATASTROPHIC = ["rm -rf /", "mkfs", "wipefs", "shred /dev/"]
DESTRUCTIVE   = ["rm -rf", "rm -fr", "rm -r", "dd if="]

def normalize(cmd):
    return " ".join(cmd.lower().strip().split())

def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and command:
        norm = normalize(command)
        for p in CATASTROPHIC:
            if p in norm:
                return "deny:catastrophic command blocked"
        for p in DESTRUCTIVE:
            if p in norm:
                return "review:destructive_command: " + command[:80]
    return "allow"
```

---

## Configuration

In `nonzeroclaw.toml`:

```toml
[security]
clash_enabled      = true
clash_policy_path  = "~/.nonzeroclaw/policy.star"
clash_profiles_dir = "~/.nonzeroclaw/profiles/"
```

When `clash_enabled = false` or no policy file is found, a **permissive**
no-op policy is used: all actions are allowed.

---

## Observability

Clash exposes in-process atomic counters scraped by the metrics endpoint:

| Counter | Description |
|---------|-------------|
| `clash_evaluations_total` | Total policy evaluations (all verdicts) |
| `clash_allows_total` | Evaluations that resulted in Allow |
| `clash_denies_total` | Evaluations that resulted in Deny |
| `clash_reviews_total` | Evaluations that triggered Review |
| `clash_review_queue_size` | Review requests currently awaiting human decision |

These counters are declared in `crates/nonzeroclaw/src/agent/loop_.rs` as
`pub static CLASH_*_TOTAL: AtomicU64` and can be read from Prometheus or
the gateway metrics endpoint.

Each evaluation also emits a structured `tracing::debug!` event:

```
clash: policy evaluation  action="tool:shell" verdict="deny" reason="destructive_command"
```

---

## Approval Flow

When a `Review` verdict fires:

1. The counter `clash_reviews_total++` and `clash_review_queue_size++`
2. The session cache (`ClashApprovalCache`) is checked for a prior
   `ApproveAlways` decision on this `(tool_name, reason_prefix)` pair
3. If cached → skip prompt, proceed with execution
4. If not cached → call `prompt_user()`:
   ```
   🔒 Clash policy: action requires approval
      Tool:   shell
      Cmd:    rm -rf /tmp/build
      Reason: destructive_command: rm -rf /tmp/build
      Approve? [Y]es / [N]o / [A]lways (remember this session): 
   ```
5. `counter clash_review_queue_size--`
6. Decision:
   - `Y` → Execute once
   - `A` → Cache `(tool_name, reason_prefix)` + Execute
   - `N` → Inject `"Review denied: {reason}"` into tool results

In **non-interactive mode** (channel runs, webhooks), Review verdicts are
auto-denied because there is no operator present to approve them.

---

## Future: OpenClaw Integration

For OpenClaw (Node.js runtime), clash policies could be applied via:

1. **Proxy mode** — OpenClaw routes tool calls through an NZC proxy that
   enforces policy before forwarding
2. **WASM compile** — Compile the clash policy engine to WASM, load in
   Node.js via wasm-bindgen
3. **Sidecar** — Separate `clashd` process; OpenClaw calls it via HTTP for
   policy checks
4. **Native addon** — Node-API binding for the Rust clash crate

For Claude Code integration:

1. **Wrapper script** — `claude` binary wrapped to preload clash
2. **MCP server** — Clash as an MCP tool that other tools call for
   pre-execution validation
3. **Native integration** — PR to upstream for a clash backend hook

---

## Testing

Run the clash integration test suite:

```bash
cd /root/projects/zeroclawed
cargo test -p nonzeroclaw --test clash_integration_tests
```

Run the approval flow integration tests:

```bash
cargo test -p nonzeroclaw --test clash_approval_flow
```

Run the clash crate's own unit tests:

```bash
cargo test -p clash
```
