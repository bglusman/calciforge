//! PolyConfig — TOML configuration loading and schema types.
//!
//! Reads from `~/.nonzeroclawed/config.toml`. Supports the full PolyConfig v2 schema
//! as defined in the NonZeroClawed v2 spec (Section 3).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

/// Top-level NonZeroClawed configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolyConfig {
    pub nonzeroclawed: PolyHeader,

    #[serde(default)]
    pub identities: Vec<Identity>,

    #[serde(default)]
    pub agents: Vec<AgentConfig>,

    #[serde(default)]
    pub routing: Vec<RoutingRule>,

    #[serde(default)]
    pub channels: Vec<ChannelConfig>,

    #[serde(default)]
    pub permissions: Option<PermissionsConfig>,

    #[serde(default)]
    pub memory: Option<MemoryConfig>,

    /// `[context]` — conversation ring buffer + injection settings.
    /// Omit from config to use defaults (buffer_size=20, inject_depth=5).
    #[serde(default)]
    pub context: ContextConfig,
}

/// `[nonzeroclawed]` header section.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolyHeader {
    pub version: u32,
}

/// An identity entry (`[[identities]]`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Identity {
    pub id: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<ChannelAlias>,
    pub role: Option<String>,
}

/// A channel alias (e.g. `{ channel = "telegram", id = "12345" }`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelAlias {
    pub channel: String,
    pub id: String,
}

/// An agent entry (`[[agents]]`).
///
/// Fields vary by `kind`:
/// - `"openclaw-http"`: uses `endpoint`, `api_key` / `auth_token`, `model`
/// - `"zeroclaw"`:      uses `endpoint`, `api_key` (required)
/// - `"cli"`:           uses `command`, `args`, `env`, `timeout_ms`
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AgentConfig {
    pub id: String,
    pub kind: String,
    /// Base URL for HTTP agents (openclaw-http, zeroclaw).
    #[serde(default)]
    pub endpoint: String,
    pub timeout_ms: Option<u64>,
    pub model: Option<String>,
    /// Legacy auth token field (openclaw-http). `api_key` takes precedence when present.
    pub auth_token: Option<String>,
    /// Per-agent API key / Bearer token. Overrides global `NONZEROCLAWED_AGENT_TOKEN`.
    pub api_key: Option<String>,
    /// OpenClaw agent lane id for kind = "openclaw-channel" (defaults to this agent id).
    #[serde(default)]
    pub openclaw_agent_id: Option<String>,
    /// Local port for OpenClaw callback replies on POST /hooks/reply (default 18797).
    #[serde(default)]
    pub reply_port: Option<u16>,
    /// Optional bearer token required on POST /hooks/reply callbacks.
    #[serde(default)]
    pub reply_auth_token: Option<String>,
    /// Path to binary for `kind = "cli"`.
    pub command: Option<String>,
    /// Argument template for `kind = "cli"`. `{message}` is substituted at dispatch time.
    pub args: Option<Vec<String>>,
    /// Environment variables for `kind = "cli"`.
    pub env: Option<HashMap<String, String>>,
    /// Optional registry metadata (ignored at runtime, used for !agents output).
    pub registry: Option<AgentRegistry>,
    /// Optional aliases for this agent (e.g. `["native", "nzc"]`).
    /// When resolving `!switch <name>`, both `id` and any alias are matched.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Agent registry metadata (inside `[agents.<id>.registry]`).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AgentRegistry {
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub specialties: Vec<String>,
    #[serde(default)]
    pub access: Vec<String>,
    #[serde(default)]
    pub primary_channels: Vec<String>,
}

/// A routing rule (`[[routing]]`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutingRule {
    pub identity: String,
    pub default_agent: String,
    #[serde(default)]
    pub allowed_agents: Vec<String>,
}

/// A channel entry (`[[channels]]`).
///
/// Supports `kind = "telegram"`, `kind = "matrix"`, `kind = "whatsapp"`, and `kind = "signal"`.
/// For Telegram: set `bot_token_file`.
/// For Matrix: set `homeserver`, `access_token_file`, `room_id`, and optionally `allowed_users`.
/// For WhatsApp: set `nzc_endpoint`, `nzc_auth_token`, `webhook_listen`, and `allowed_numbers`.
/// For Signal: set `nzc_endpoint`, `nzc_auth_token`, `webhook_listen`, and `allowed_numbers` (same fields as WhatsApp).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChannelConfig {
    pub kind: String,
    /// Path to bot token file (Telegram only).
    pub bot_token_file: Option<String>,
    #[serde(default)]
    pub enabled: bool,

    // --- Matrix-specific fields ---
    /// Matrix homeserver URL, e.g. `"https://matrix.org"`.
    pub homeserver: Option<String>,

    /// Path to a file containing the Matrix bot access token.
    /// Generate with: `curl -XPOST 'https://matrix.org/_matrix/client/v3/login' -d '{"type":"m.login.password","user":"@bot:matrix.org","password":"..."}'`
    pub access_token_file: Option<String>,

    /// Matrix room ID the bot should join and listen in, e.g. `"!abc123:matrix.org"`.
    pub room_id: Option<String>,

    /// List of Matrix user IDs allowed to send commands, e.g. `["@brian:matrix.org"]`.
    /// If empty, all room members can interact (not recommended).
    #[serde(default)]
    pub allowed_users: Vec<String>,

    // --- WhatsApp/Signal-specific fields (shared) ---
    /// NZC (NonZeroClaw) / OpenClaw gateway endpoint that owns the WA Web or Signal session.
    /// NonZeroClawed will POST reply messages to `{nzc_endpoint}/tools/invoke`.
    /// Example: `"http://127.0.0.1:18789"` (local OpenClaw) or
    ///          `"http://10.0.0.10:18789"` (remote Lucien/NZC instance).
    pub nzc_endpoint: Option<String>,

    /// Bearer token for the NZC / OpenClaw gateway.
    pub nzc_auth_token: Option<String>,

    /// HTTP address to listen on for incoming webhook POSTs from NZC.
    /// Defaults to `"0.0.0.0:18795"`.
    pub webhook_listen: Option<String>,

    /// URL path NonZeroClawed registers for incoming WhatsApp webhooks.
    /// Defaults to `"/webhooks/whatsapp"`.
    pub webhook_path: Option<String>,

    /// Optional HMAC-SHA256 secret for `X-Hub-Signature-256` webhook verification.
    /// Leave unset to skip signature checking (not recommended for production).
    pub webhook_secret: Option<String>,

    /// Allowed sender phone numbers in E.164 format, e.g. `["+15555550001"]`.
    /// Use `"*"` to allow any number (not recommended).
    /// Must correspond to identity aliases with `channel = "whatsapp"` or `channel = "signal"`.
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

/// `[permissions]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionsConfig {
    pub default: Option<String>,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

/// A permission rule.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionRule {
    pub identity: String,
    pub effect: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// `[memory]` section (no-op for now, parsed so config doesn't break).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MemoryConfig {
    pub pre_read_hook: Option<String>,
    pub post_write_hook: Option<String>,
    pub store: Option<String>,
    pub store_path: Option<String>,
}

/// `[context]` section — conversation context ring buffer settings.
///
/// Omitting the section from config.toml uses all defaults (enabled, 20/5).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextConfig {
    /// Maximum exchanges to retain per chat.  Older exchanges are evicted.
    /// Default: 20.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// How many recent unseen exchanges to prepend when dispatching to an agent.
    /// Set to 0 to disable injection.  Default: 5.
    #[serde(default = "default_inject_depth")]
    pub inject_depth: usize,
}

fn default_buffer_size() -> usize {
    20
}
fn default_inject_depth() -> usize {
    5
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            buffer_size: default_buffer_size(),
            inject_depth: default_inject_depth(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load the PolyConfig from `~/.nonzeroclawed/config.toml`.
pub fn load_config() -> Result<PolyConfig> {
    let path = config_path()?;
    load_config_from(&path)
}

/// Load the PolyConfig from an explicit path.
pub fn load_config_from(path: &PathBuf) -> Result<PolyConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file: {}", path.display()))?;
    let config: PolyConfig =
        toml::from_str(&raw).with_context(|| format!("parsing config file: {}", path.display()))?;
    Ok(config)
}

/// Returns the canonical config file path: `~/.nonzeroclawed/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    let home = home::home_dir().context("could not determine home directory")?;
    Ok(home.join(".nonzeroclawed").join("config.toml"))
}

/// Expand a `~`-prefixed path using the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = home::home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
[nonzeroclawed]
version = 2

[[identities]]
id = "brian"
aliases = [{ channel = "telegram", id = "8465871195" }]
role = "owner"

[[identities]]
id = "david"
display_name = "David"
aliases = [{ channel = "telegram", id = "15555550002" }]
role = "user"

[[agents]]
id = "librarian"
kind = "openclaw-http"
endpoint = "http://10.0.0.20:18789"
timeout_ms = 120000
registry = { display_name = "Librarian", specialties = ["general", "homelab-ops"] }
aliases = ["lib", "main"]

[[agents]]
id = "zeroclaw"
kind = "zeroclaw"
endpoint = "http://127.0.0.1:18792"
api_key = "zc_4f5c220eec86bedf6e7a9fb99e26b3831811f090fd225b6bbe3bbc2626a3dd86"
timeout_ms = 90000

[[agents]]
id = "ironclaw"
kind = "cli"
command = "/usr/local/bin/ironclaw"
args = ["run", "-m", "{message}"]
timeout_ms = 60000
env = { "LLM_BACKEND" = "openai_compatible", "LLM_MODEL" = "kimi-k2.5" }

[[agents]]
id = "claude-code"
kind = "acp"
command = "claude"
args = ["--acp"]
model = "claude-sonnet-4-5"
timeout_ms = 300000
aliases = ["cc", "claude"]
registry = { display_name = "Claude Code", specialties = ["coding", "refactoring"] }

[[routing]]
identity = "brian"
default_agent = "librarian"

[[routing]]
identity = "david"
default_agent = "librarian"
allowed_agents = ["librarian"]

[[channels]]
kind = "telegram"
bot_token_file = "~/.nonzeroclawed/secrets/telegram-token"
enabled = true

[memory]
pre_read_hook = "none"
post_write_hook = "none"
"#;

    #[test]
    fn test_parse_sample_config() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        assert_eq!(cfg.nonzeroclawed.version, 2);
        assert_eq!(cfg.identities.len(), 2);
        assert_eq!(cfg.identities[0].id, "brian");
        assert_eq!(cfg.identities[1].id, "david");
        assert_eq!(cfg.agents.len(), 4); // librarian + zeroclaw + ironclaw + claude-code
        assert_eq!(cfg.agents[0].id, "librarian");
        assert_eq!(cfg.agents[0].endpoint, "http://10.0.0.20:18789");
        assert_eq!(cfg.agents[0].timeout_ms, Some(120000));
        assert_eq!(cfg.routing.len(), 2);
        assert_eq!(cfg.routing[0].default_agent, "librarian");
        assert_eq!(cfg.channels.len(), 1);
        assert_eq!(cfg.channels[0].kind, "telegram");
        assert!(cfg.channels[0].enabled);
    }

    #[test]
    fn test_identity_aliases() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let brian = &cfg.identities[0];
        assert_eq!(brian.aliases.len(), 1);
        assert_eq!(brian.aliases[0].channel, "telegram");
        assert_eq!(brian.aliases[0].id, "8465871195");
    }

    #[test]
    fn test_routing_allowed_agents() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        // brian's routing has no allowed_agents (defaults to empty)
        assert!(cfg.routing[0].allowed_agents.is_empty());
        // david's routing specifies allowed_agents
        assert_eq!(cfg.routing[1].allowed_agents, vec!["librarian"]);
    }

    #[test]
    fn test_expand_tilde() {
        let p = expand_tilde("~/.nonzeroclawed/secrets/telegram-token");
        assert!(p.to_string_lossy().contains(".nonzeroclawed"));
        assert!(!p.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn test_version_field() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        assert_eq!(cfg.nonzeroclawed.version, 2, "must be version 2");
    }

    #[test]
    fn test_optional_fields_absent() {
        let minimal = r#"
[nonzeroclawed]
version = 2
"#;
        let cfg: PolyConfig = toml::from_str(minimal).expect("parse failed");
        assert!(cfg.identities.is_empty());
        assert!(cfg.agents.is_empty());
        assert!(cfg.routing.is_empty());
        assert!(cfg.channels.is_empty());
        assert!(cfg.permissions.is_none());
        assert!(cfg.memory.is_none());
    }

    #[test]
    fn test_zeroclaw_agent_parses() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let zc = cfg
            .agents
            .iter()
            .find(|a| a.id == "zeroclaw")
            .expect("zeroclaw agent missing");
        assert_eq!(zc.kind, "zeroclaw");
        assert_eq!(zc.endpoint, "http://127.0.0.1:18792");
        assert_eq!(
            zc.api_key.as_deref(),
            Some("zc_4f5c220eec86bedf6e7a9fb99e26b3831811f090fd225b6bbe3bbc2626a3dd86")
        );
        assert_eq!(zc.timeout_ms, Some(90000));
        assert!(zc.command.is_none());
        assert!(zc.env.is_none());
    }

    #[test]
    fn test_cli_agent_parses() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let ic = cfg
            .agents
            .iter()
            .find(|a| a.id == "ironclaw")
            .expect("ironclaw agent missing");
        assert_eq!(ic.kind, "cli");
        assert_eq!(ic.command.as_deref(), Some("/usr/local/bin/ironclaw"));
        assert_eq!(
            ic.args.as_deref(),
            Some(&["run".to_string(), "-m".to_string(), "{message}".to_string()][..])
        );
        let env = ic.env.as_ref().expect("env should be set");
        assert_eq!(env["LLM_BACKEND"], "openai_compatible");
        assert_eq!(env["LLM_MODEL"], "kimi-k2.5");
        assert!(ic.api_key.is_none());
        assert!(ic.endpoint.is_empty());
    }

    #[test]
    fn test_registry_metadata_parses() {
        // Registry uses inline table syntax (registry = { ... }) to be parseable
        // as a Vec<Agent> field. The production config's [agents.id.registry] dotted
        // table syntax is valid TOML but is silently ignored by the toml crate when
        // deserializing array-of-tables — it's documentation only.
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let lib = cfg
            .agents
            .iter()
            .find(|a| a.id == "librarian")
            .expect("librarian missing");
        let reg = lib
            .registry
            .as_ref()
            .expect("registry should be present (inline table)");
        assert_eq!(reg.display_name.as_deref(), Some("Librarian"));
        assert!(reg.specialties.contains(&"general".to_string()));
    }

    #[test]
    fn test_memory_config_parses() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let mem = cfg.memory.as_ref().expect("memory section should parse");
        assert_eq!(mem.pre_read_hook.as_deref(), Some("none"));
        assert_eq!(mem.post_write_hook.as_deref(), Some("none"));
    }

    #[test]
    fn test_context_config_defaults_when_omitted() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        // SAMPLE_CONFIG has no [context] section — defaults must kick in
        assert_eq!(cfg.context.buffer_size, 20);
        assert_eq!(cfg.context.inject_depth, 5);
    }

    #[test]
    fn test_context_config_parses_explicit() {
        let raw = r#"
[nonzeroclawed]
version = 2

[context]
buffer_size = 10
inject_depth = 3
"#;
        let cfg: PolyConfig = toml::from_str(raw).expect("parse failed");
        assert_eq!(cfg.context.buffer_size, 10);
        assert_eq!(cfg.context.inject_depth, 3);
    }

    #[test]
    fn test_context_config_partial_override() {
        // Only override one field; the other should use its default
        let raw = r#"
[nonzeroclawed]
version = 2

[context]
inject_depth = 0
"#;
        let cfg: PolyConfig = toml::from_str(raw).expect("parse failed");
        assert_eq!(
            cfg.context.buffer_size, 20,
            "buffer_size should default to 20"
        );
        assert_eq!(
            cfg.context.inject_depth, 0,
            "inject_depth should be 0 (disabled)"
        );
    }

    #[test]
    fn test_agent_aliases_parse() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let lib = cfg
            .agents
            .iter()
            .find(|a| a.id == "librarian")
            .expect("librarian missing");
        assert_eq!(lib.aliases, vec!["lib".to_string(), "main".to_string()]);
    }

    #[test]
    fn test_agent_aliases_default_empty() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        // zeroclaw agent has no aliases field — should default to empty
        let zc = cfg
            .agents
            .iter()
            .find(|a| a.id == "zeroclaw")
            .expect("zeroclaw missing");
        assert!(
            zc.aliases.is_empty(),
            "missing aliases field should default to empty vec"
        );
    }

    #[test]
    fn test_acp_agent_parses() {
        let cfg: PolyConfig = toml::from_str(SAMPLE_CONFIG).expect("parse failed");
        let cc = cfg
            .agents
            .iter()
            .find(|a| a.id == "claude-code")
            .expect("claude-code agent missing");
        assert_eq!(cc.kind, "acp");
        assert_eq!(cc.command.as_deref(), Some("claude"));
        assert_eq!(cc.args.as_deref(), Some(&["--acp".to_string()][..]));
        assert_eq!(cc.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(cc.timeout_ms, Some(300000));
        assert_eq!(cc.aliases, vec!["cc".to_string(), "claude".to_string()]);
        let reg = cc.registry.as_ref().expect("registry should be present");
        assert_eq!(reg.display_name.as_deref(), Some("Claude Code"));
        assert!(reg.specialties.contains(&"coding".to_string()));
    }

    #[test]
    fn test_openclaw_agent_api_key_field() {
        let raw = r#"
[nonzeroclawed]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-http"
endpoint = "http://10.0.0.60:18790"
api_key = "REPLACE_WITH_AUTH_TOKEN"
timeout_ms = 60000
"#;
        let cfg: PolyConfig = toml::from_str(raw).expect("parse failed");
        let agent = &cfg.agents[0];
        assert_eq!(agent.api_key.as_deref(), Some("REPLACE_WITH_AUTH_TOKEN"));
        assert!(agent.auth_token.is_none());
    }
}
