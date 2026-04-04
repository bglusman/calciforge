# OpenClaw Config Schema Versioning

_Research date: 2026-03-30_
_Sources: live `openclaw --version`, `openclaw config` commands, docs inspection_

---

## Version Detection

### Method 1: CLI (if SSH access available)
```bash
openclaw --version
# Output: OpenClaw 2026.3.13 (61d171a)
```

Returns: `<year>.<month>.<patch>` semver + short git commit hash.

**Librarian instance**: `2026.3.13 (61d171a)`

### Method 2: Gateway HTTP (no CLI needed)
```bash
curl -H "Authorization: Bearer <token>" http://<claw>:18789/health
# Output: { "ok": true, "status": "live" }
```

The `/health` endpoint returns liveness but **not** the version. There is no `/version` HTTP endpoint — it returns 404. **Version is not exposed over HTTP in the current schema.**

### Method 3: Config File Inspection
```json
// openclaw.json — written by every wizard run
{
  "meta": {
    "lastTouchedVersion": "2026.3.13",
    "lastTouchedAt": "2026-03-29T15:00:31.143Z"
  }
}
```

The `meta.lastTouchedVersion` field in `openclaw.json` records the version that last wrote the config. This is **not a guarantee** the running gateway is at that version (the gateway might have been updated since without running a wizard), but it gives a baseline.

**For NonZeroClawed's adapter installer**: reading `meta.lastTouchedVersion` from the config file is the most reliable version signal without requiring SSH or a new CLI invocation.

---

## Schema Validation

### `openclaw config` CLI

```bash
openclaw config        # Starts interactive wizard (prompts!)
openclaw config get    # Read config values
openclaw config set    # Write config values
openclaw config validate   # Validate current config
```

**`openclaw config schema`** — does NOT exist. Returns:
```
error: too many arguments for 'config'. Expected 0 arguments but got 1.
```

There is **no machine-readable JSON Schema export command** in the current CLI.

### Where the Schema Lives

The config schema is embedded in the OpenClaw binary (Node.js package at `/usr/lib/node_modules/openclaw/`). It's not exposed as a standalone JSON Schema file. The `plugins/manifest.md` docs confirm that **plugin config schemas are JSON Schema objects**, and the core config is validated internally.

**Finding the schema programmatically**:
```bash
find /usr/lib/node_modules/openclaw -name "*.json" | xargs grep -l '"configSchema"\|"type.*object"' 2>/dev/null | head -10
# or
find /usr/lib/node_modules/openclaw/dist -name "*.js" | head -5
```

The schema is likely compiled into the dist bundle — not easily extractable without source access.

### `openclaw doctor`

```bash
openclaw doctor         # Read-only health checks + quick fixes
openclaw doctor --fix   # Apply auto-fixes for known issues
```

`doctor` validates config against the running version's schema and reports errors. This is the **safest way to validate a proposed config change**: write the new config → run `doctor` → check for errors → if clean, restart gateway.

---

## Schema Versioning Strategy for NonZeroClawed

### What We Know

1. **OpenClaw uses calendar versioning**: `YYYY.M.patch` (e.g. `2026.3.13`)
2. **No machine-readable schema endpoint**: schema is embedded in binary
3. **Version recorded in config**: `meta.lastTouchedVersion`
4. **Doctor command validates**: safest runtime validation mechanism
5. **Config is JSON5**: allows comments, trailing commas — not strict JSON

### Proposed NonZeroClawed Compatibility Matrix Approach

Since there's no schema introspection API, NonZeroClawed must maintain a **hardcoded compatibility matrix**:

```rust
// In NonZeroClawed's openclaw adapter
pub struct OpenClawCompatibility {
    version: String,      // "2026.3.13"
    commit: Option<String>, // "61d171a"
    known_config_keys: Vec<String>,
    safe_to_add: Vec<String>,
    requires_plugin: Vec<String>,
}

static COMPATIBILITY_MATRIX: &[OpenClawCompatibility] = &[
    OpenClawCompatibility {
        version: "2026.3.*".to_string(),
        known_config_keys: vec![
            "gateway.http.endpoints.chatCompletions.enabled",
            "hooks.enabled",
            "hooks.token",
            "hooks.path",
            "hooks.allowedAgentIds",
            "plugins.enabled",
            "plugins.allow",
            "plugins.entries.*",
        ],
        safe_to_add: vec![
            "gateway.http.endpoints.chatCompletions.enabled",
            "hooks.enabled",
            "hooks.token",
        ],
        requires_plugin: vec![
            "channels.nonzeroclawed",
        ],
    },
];
```

### Safe Adapter Install Protocol (updated from plan)

```
1. Read target openclaw.json
2. Extract meta.lastTouchedVersion → "2026.3.13"
3. Parse: year=2026, month=3, patch=13
4. Look up compatibility matrix entry for "2026.3.*"
5. If version is UNKNOWN → refuse with explanation
6. If version is KNOWN:
   a. Backup openclaw.json to nonzeroclawed-backup-<timestamp>.json
   b. Generate proposed changes (only from safe_to_add list for this version)
   c. Write changes to a TEMP file
   d. Run: openclaw doctor (via SSH or subprocess)
   e. If doctor passes → mv temp → openclaw.json
   f. Run: openclaw gateway restart (or signal HUP)
   g. Poll /health for 30s
   h. If /health returns OK → success, store config hash
   i. If /health fails → restore backup, alert operator
```

### Version Fields to Read

The complete version detection sequence for NonZeroClawed:

```rust
// Priority order for version detection
fn detect_openclaw_version(config: &OpenClawConfig, ssh: Option<&SshClient>) -> OpenClawVersion {
    // 1. Try SSH: most accurate
    if let Some(ssh) = ssh {
        if let Ok(out) = ssh.run("openclaw --version") {
            return parse_version_string(&out); // "OpenClaw 2026.3.13 (61d171a)"
        }
    }
    
    // 2. Fall back to config meta field
    if let Some(meta) = &config.meta {
        if let Some(v) = &meta.last_touched_version {
            return parse_version_string(v); // "2026.3.13"
        }
    }
    
    // 3. Unknown — refuse to proceed
    OpenClawVersion::Unknown
}
```

---

## Known Schema Facts for `2026.3.13`

From documentation analysis and live inspection:

### Safe to add without plugin (tested/documented):
- `gateway.http.endpoints.chatCompletions.enabled: true`
- `gateway.http.endpoints.responses.enabled: true`
- `hooks.enabled: true`
- `hooks.token: "..."`
- `hooks.path: "/hooks"`
- `hooks.allowedAgentIds: [...]`

### Requires plugin installed first:
- `channels.nonzeroclawed.*` — NonZeroClawed channel plugin must be installed and discoverable
- `channels.mattermost.*` — requires `@openclaw/mattermost` plugin
- Any `channels.<id>` where `<id>` is not a built-in channel name

### Known to break gateway if wrong:
- `channels.*` entries with incorrect structure (caused 2026-03-30 incident)
- `plugins.entries.<id>` where `<id>` plugin is not installed or has broken manifest
- Any field not in the schema — treated as validation error

### Built-in channel names (safe to configure without plugins):
`whatsapp`, `telegram`, `discord`, `slack`, `signal`, `googlechat`, `msteams`, `irc`, `imessage`, `bluebubbles`, `matrix`, `nostr`, `feishu`, `line`, `nextcloud-talk`, `synology-chat`, `twitch`, `zalo`, `tlon`

---

## No Machine-Readable Schema: Implications for NonZeroClawed

**The core problem**: NonZeroClawed cannot dynamically query "what fields does this version support?" It must maintain its own compatibility matrix.

**Mitigation strategies**:

1. **Conservative additions only**: Only add fields from NonZeroClawed's allowlist for known versions. Never add fields not on the list.

2. **Doctor validation before commit**: Always run `openclaw doctor` on the proposed config before applying. Doctor will catch unknown fields even if NonZeroClawed's matrix missed them.

3. **Version-gated features**: If target version is older than what a feature requires, disable that feature and warn.

4. **Schema snapshot approach**: When running the installer, capture a diff of `openclaw.json` before and after. If the diff includes unexpected fields, abort.

5. **OpenClaw SDK future watch**: If OpenClaw ever adds `openclaw config schema --json` or a `GET /schema` endpoint, update the installer to use it. The current absence is a gap.

---

## Sources

- `openclaw --version` on live Librarian instance
- `openclaw config schema` attempt → error (confirmed missing)
- `openclaw --help` full command list inspection
- `/usr/lib/node_modules/openclaw/docs/plugins/manifest.md`
- `/usr/lib/node_modules/openclaw/docs/gateway/configuration-reference.md`
- `~/.openclaw/openclaw.json` live inspection (`meta.lastTouchedVersion`)
- Gateway probe: no `/version` HTTP endpoint found
