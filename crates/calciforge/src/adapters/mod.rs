//! Agent adapter trait and concrete implementations.
//!
//! Each adapter handles the protocol-level details of dispatching a message
//! to a downstream agent. Calciforge's router calls `adapter.dispatch(text)` —
//! it never touches agent internals directly.
//!
//! # Adapters
//!
//! - [`OpenClawHttpAdapter`] — POST `/v1/chat/completions` (OpenAI-compat HTTP)
//! - [`ZeroClawAdapter`] — POST `/webhook` with `{"message": text}` (custom protocol)
//! - [`CliAdapter`] — spawn binary, pass `-m "text"`, read stdout
//!
//! # Usage
//!
//! ```no_run
//! use calciforge::adapters::{build_adapter, AgentAdapter};
//! // build_adapter reads kind from AgentConfig and returns a Box<dyn AgentAdapter>
//! ```

use async_trait::async_trait;
use std::fmt;

pub mod acp;
pub mod acpx;
pub mod cli;
pub mod codex_cli;
pub mod openclaw;
pub mod openclaw_channel;
pub mod openclaw_native;
pub mod zeroclaw;
pub mod zeroclaw_native;

pub use acp::AcpAdapter;
pub use acpx::AcpxAdapter;
pub use cli::CliAdapter;
pub use codex_cli::CodexCliAdapter;
pub use openclaw::{OpenClawHttpAdapter, ZeroClawHttpAdapter};
pub use openclaw_channel::OpenClawChannelAdapter;
pub use openclaw_native::OpenClawNativeAdapter;
pub use zeroclaw::ZeroClawAdapter;
pub use zeroclaw_native::ZeroClawNativeAdapter;

use crate::config::AgentConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Approval request embedded in a ZeroClaw webhook response when a Clash `Review`
/// verdict fires.  Bubbled up through `AdapterError::ApprovalPending` so the
/// Calciforge router can send the approval notification to the user.
#[derive(Debug, Clone)]
pub struct ZeroClawApprovalRequest {
    pub request_id: String,
    pub reason: String,
    pub command: String,
}

/// Errors returned by agent adapters.
#[derive(Debug)]
pub enum AdapterError {
    /// The request timed out.
    Timeout,
    /// The agent is unreachable (network error, service down, etc.).
    Unavailable(String),
    /// The agent returned an unexpected response format.
    Protocol(String),
    /// The agent loop paused for human approval (Clash `Review` verdict).
    /// The router should notify the user and not send any other reply yet.
    ApprovalPending(ZeroClawApprovalRequest),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Timeout => write!(f, "agent request timed out"),
            AdapterError::Unavailable(msg) => write!(f, "agent unavailable: {}", msg),
            AdapterError::Protocol(msg) => write!(f, "protocol error: {}", msg),
            AdapterError::ApprovalPending(req) => write!(
                f,
                "🔒 Approval pending — request_id={}, command={}",
                req.request_id, req.command
            ),
        }
    }
}

impl std::error::Error for AdapterError {}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Optional sender context forwarded to downstream agents.
///
/// Most adapters ignore sender fields and just use `message`.
/// `ZeroClawHttpAdapter` forwards `sender` to ZeroClaw so it can maintain
/// per-sender conversation history keyed on the resolved identity name.
#[derive(Debug, Clone)]
pub struct DispatchContext<'a> {
    /// The user message text.
    pub message: &'a str,
    /// Resolved identity name from Calciforge (e.g. "brian", "renee").
    /// This is the identity id, not a phone number or channel-specific id.
    pub sender: Option<&'a str>,
    /// Optional per-request model override (used by alloy routing).
    #[allow(dead_code)]
    pub model_override: Option<&'a str>,
}

impl<'a> DispatchContext<'a> {
    /// Create a context with only a message and no sender info.
    pub fn message_only(message: &'a str) -> Self {
        Self {
            message,
            sender: None,
            model_override: None,
        }
    }
}

/// Runtime model/provider status reported by an adapter.
///
/// Adapters that can query their underlying agent's runtime state
/// return this from `get_runtime_status()`. For alloy providers,
/// constituents list the constituent providers and models.
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// The provider kind (e.g. "openai", "ollama", "alloy", "openclaw")
    pub provider: String,
    /// The model name or alloy alias (e.g. "gpt-5-mini", "fast-alloy")
    pub model: String,
    /// If this is an alloy, the constituent providers and their models
    pub alloy_constituents: Option<Vec<(String, String)>>,
    /// Which constituent was selected for the most recent request (if known)
    pub _last_selected: Option<(String, String)>,
}

/// Common interface for all agent adapters.
///
/// Implementations are `Send + Sync` so they can be wrapped in `Arc` and
/// shared across async tasks.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Dispatch a message to the agent and return its text response.
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError>;

    /// Dispatch with optional sender context.
    ///
    /// The default implementation ignores sender fields and delegates to
    /// `dispatch(ctx.message)`. Adapters that support sender-aware routing
    /// (e.g. `ZeroClawHttpAdapter`) override this.
    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        self.dispatch(ctx.message).await
    }

    /// Short name for logs and `!agents` output (e.g. "openclaw-http", "zeroclaw", "cli").
    fn kind(&self) -> &'static str;

    /// Query the underlying agent's runtime model/provider status.
    ///
    /// Default implementation returns `None` — adapters that support
    /// runtime introspection (e.g. ZeroClaw) override this.
    async fn get_runtime_status(&self) -> Option<RuntimeStatus> {
        None
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build a concrete `AgentAdapter` from an `AgentConfig`.
///
/// Returns an error if the `kind` is unknown or required config fields are
/// missing.
///
/// # Adapter kinds
///
/// | `kind`             | Protocol            | Session continuity | Native commands |
/// |--------------------|---------------------|--------------------|-----------------|
/// | `openclaw-http`    | `/v1/chat/completions` (SSE) | ⚠️ via header | ❌ |
/// | `openclaw-native`  | `/hooks/agent`      | ✅ native sessionKey | ✅ |
/// | `zeroclaw-http`    | `/webhook`          | ❌ stateless        | ✅ |
/// | `zeroclaw-native`  | `/webhook` + history | ✅ in-process ring buffer | ✅ |
/// | `zeroclaw`         | `/webhook`          | per-ZeroClaw-config | n/a |
/// | `cli`              | subprocess stdin    | ❌ one-shot         | n/a |
/// | `codex-cli`        | `codex exec`        | ❌ one-shot         | n/a |
/// | `acp`              | SACP stdio          | ✅ persistent proc  | n/a |
/// | `acpx`             | acpx CLI            | ✅ acpx sessions    | n/a |
pub fn build_adapter(agent: &AgentConfig) -> Result<Box<dyn AgentAdapter>, String> {
    let agent_token = || resolve_agent_token(agent, true);
    let agent_token_no_env = || resolve_agent_token(agent, false);

    match agent.kind.as_str() {
        "openclaw-http" => {
            let token = agent_token()?;
            Ok(Box::new(OpenClawHttpAdapter::new_with_agent_id(
                agent.endpoint.clone(),
                token,
                agent.model.clone(),
                agent.timeout_ms,
                &agent.id,
            )))
        }
        "openclaw-channel" => {
            let token = agent_token()?;
            let openclaw_agent_id = agent
                .openclaw_agent_id
                .clone()
                .unwrap_or_else(|| agent.id.clone());
            Ok(Box::new(OpenClawChannelAdapter::new(
                agent.endpoint.clone(),
                token,
                openclaw_agent_id,
                agent.reply_port,
                agent.reply_auth_token.clone(),
                agent.timeout_ms,
            )))
        }
        "zeroclaw-http" => {
            let token = agent_token_no_env()?;
            Ok(Box::new(ZeroClawHttpAdapter::new(
                agent.endpoint.clone(),
                token,
                agent.timeout_ms,
            )))
        }
        // ── New native adapters ─────────────────────────────────────────────
        //
        // `openclaw-native`: uses OpenClaw's `/hooks/agent` endpoint so that
        // native commands (/status, !approve, etc.) are handled by the OpenClaw
        // pipeline rather than forwarded to the LLM.  Session continuity is
        // maintained via a stable `sessionKey` derived from agent_id + sender.
        //
        // Requires `hooks.enabled = true` in your OpenClaw config, and optionally
        // `hooks.allowRequestSessionKey = true` + `allowedSessionKeyPrefixes = ["calciforge:"]`
        // for full session continuity.
        //
        // `api_key` / `auth_token` should be the `hooks.token` (NOT the gateway token).
        "openclaw-native" => {
            let token = agent_token()?;
            // Use openclaw_agent_id if set, otherwise fall back to agent.id.
            // This allows a Calciforge agent named "openclaw-max" to route to
            // OpenClaw's "david" agent without renaming the Calciforge-side entry.
            let target_agent_id = agent
                .openclaw_agent_id
                .clone()
                .unwrap_or_else(|| agent.id.clone());
            Ok(Box::new(OpenClawNativeAdapter::new(
                agent.endpoint.clone(),
                token,
                target_agent_id,
                None, // hooks_path — use default "/hooks"
                agent.timeout_ms,
            )))
        }
        // `zeroclaw-native`: wraps `ZeroClawHttpAdapter` with an in-process conversation
        // history ring buffer.  Each request includes the prior (user, assistant)
        // turns as a preamble so the ZeroClaw agent has full conversational context.
        //
        // `ApprovalPending` responses are handled gracefully — the pending turn is
        // not recorded until the approval is resolved.
        "zeroclaw-native" => {
            let token = agent_token_no_env()?;
            Ok(Box::new(ZeroClawNativeAdapter::new(
                agent.endpoint.clone(),
                token,
                agent.timeout_ms,
            )))
        }
        "zeroclaw" => {
            let api_key = agent_token_no_env()?;
            if api_key.is_empty() {
                return Err(format!(
                    "agent '{}': kind='zeroclaw' requires api_key or api_key_file",
                    agent.id
                ));
            }
            Ok(Box::new(ZeroClawAdapter::new(
                agent.endpoint.clone(),
                api_key,
                agent.timeout_ms,
            )))
        }
        "cli" => {
            let command = agent
                .command
                .clone()
                .ok_or_else(|| format!("agent '{}': kind='cli' requires command", agent.id))?;
            Ok(Box::new(CliAdapter::with_model(
                command,
                agent.args.clone(),
                agent.env.clone().unwrap_or_default(),
                agent.model.clone(),
                agent.timeout_ms,
            )))
        }
        "codex-cli" => Ok(Box::new(CodexCliAdapter::new(
            agent.command.clone(),
            agent.args.clone(),
            agent.model.clone(),
            agent.env.clone(),
            agent.timeout_ms,
        ))),
        "acp" => {
            let command = agent
                .command
                .clone()
                .ok_or_else(|| format!("agent '{}': kind='acp' requires command", agent.id))?;
            Ok(Box::new(AcpAdapter::new(
                command,
                agent.args.clone(),
                agent.env.clone().unwrap_or_default(),
                agent.model.clone(),
                agent.timeout_ms,
            )))
        }
        "acpx" => {
            let agent_name = agent.command.clone().ok_or_else(|| {
                format!(
                    "agent '{}': kind='acpx' requires command (agent name)",
                    agent.id
                )
            })?;
            Ok(Box::new(AcpxAdapter::new(
                agent_name,
                agent.args.clone(),
                agent.env.clone(),
                agent.timeout_ms,
            )))
        }
        other => Err(format!("unknown agent kind: '{}'", other)),
    }
}

fn resolve_agent_token(agent: &AgentConfig, allow_env: bool) -> Result<String, String> {
    if let Some(path) = &agent.api_key_file {
        let path = crate::config::expand_tilde(&path.to_string_lossy());
        let token = std::fs::read_to_string(path)
            .map_err(|e| format!("agent '{}': failed to read api_key_file: {e}", agent.id))?;
        return Ok(token.trim().to_string());
    }

    if let Some(token) = &agent.api_key {
        return Ok(token.clone());
    }

    if let Some(token) = &agent.auth_token {
        return Ok(token.clone());
    }

    if allow_env {
        return Ok(std::env::var("CALCIFORGE_AGENT_TOKEN").unwrap_or_default());
    }

    Ok(String::new())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use std::collections::HashMap;

    fn openclaw_agent() -> AgentConfig {
        AgentConfig {
            id: "test-openclaw".to_string(),
            kind: "openclaw-http".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            timeout_ms: Some(5000),
            model: Some("openclaw:main".to_string()),
            auth_token: Some("tok123".to_string()),
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    fn zeroclaw_agent() -> AgentConfig {
        AgentConfig {
            id: "test-zeroclaw".to_string(),
            kind: "zeroclaw".to_string(),
            endpoint: "http://127.0.0.1:18792".to_string(),
            timeout_ms: Some(5000),
            model: None,
            auth_token: None,
            api_key: Some("zc_abc123".to_string()),
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    fn cli_agent() -> AgentConfig {
        AgentConfig {
            id: "test-cli".to_string(),
            kind: "cli".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(5000),
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: Some("/usr/local/bin/ironclaw".to_string()),
            args: Some(vec![
                "run".to_string(),
                "-m".to_string(),
                "{message}".to_string(),
            ]),
            env: Some({
                let mut m = HashMap::new();
                m.insert("LLM_BACKEND".to_string(), "openai_compatible".to_string());
                m
            }),
            registry: None,
            aliases: vec![],
        }
    }

    #[test]
    fn test_build_openclaw_adapter() {
        let agent = openclaw_agent();
        let adapter = build_adapter(&agent).expect("should build openclaw adapter");
        assert_eq!(adapter.kind(), "openclaw-http");
    }

    #[test]
    fn test_build_zeroclaw_adapter() {
        let agent = zeroclaw_agent();
        let adapter = build_adapter(&agent).expect("should build zeroclaw adapter");
        assert_eq!(adapter.kind(), "zeroclaw");
    }

    #[test]
    fn test_build_cli_adapter() {
        let agent = cli_agent();
        let adapter = build_adapter(&agent).expect("should build cli adapter");
        assert_eq!(adapter.kind(), "cli");
    }

    #[test]
    fn test_build_codex_cli_adapter() {
        let agent = AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(600_000),
            model: Some("gpt-5.5".to_string()),
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec!["gpt".to_string()],
        };
        let adapter = build_adapter(&agent).expect("should build codex-cli adapter");
        assert_eq!(adapter.kind(), "codex-cli");
    }

    #[test]
    fn test_build_unknown_kind_returns_error() {
        let agent = AgentConfig {
            id: "test".to_string(),
            kind: "not-a-real-kind".to_string(),
            endpoint: "http://localhost".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let result = build_adapter(&agent);
        assert!(result.is_err());
        let err = result.err().expect("should be Err");
        assert!(err.contains("unknown agent kind"), "got: {}", err);
    }

    #[test]
    fn test_build_zeroclaw_missing_api_key_returns_error() {
        let agent = AgentConfig {
            id: "zc".to_string(),
            kind: "zeroclaw".to_string(),
            endpoint: "http://127.0.0.1:18792".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None, // missing!
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let result = build_adapter(&agent);
        assert!(result.is_err());
        let err = result.err().expect("should be Err");
        assert!(err.contains("api_key"), "got: {}", err);
    }

    fn acp_agent() -> AgentConfig {
        AgentConfig {
            id: "test-acp".to_string(),
            kind: "acp".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(60000),
            model: Some("claude-sonnet-4-5".to_string()),
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: Some("claude".to_string()),
            args: Some(vec!["--acp".to_string()]),
            env: None,
            registry: None,
            aliases: vec!["cc".to_string()],
        }
    }

    #[test]
    fn test_build_acp_adapter() {
        let agent = acp_agent();
        let adapter = build_adapter(&agent).expect("should build acp adapter");
        assert_eq!(adapter.kind(), "acp");
    }

    #[test]
    fn test_build_acp_missing_command_returns_error() {
        let agent = AgentConfig {
            id: "acp-no-cmd".to_string(),
            kind: "acp".to_string(),
            endpoint: String::new(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None, // missing!
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let result = build_adapter(&agent);
        assert!(result.is_err());
        let err = result.err().expect("should be Err");
        assert!(err.contains("command"), "got: {}", err);
    }

    #[test]
    fn test_build_cli_missing_command_returns_error() {
        let agent = AgentConfig {
            id: "cli".to_string(),
            kind: "cli".to_string(),
            endpoint: String::new(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None, // missing!
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let result = build_adapter(&agent);
        assert!(result.is_err());
        let err = result.err().expect("should be Err");
        assert!(err.contains("command"), "got: {}", err);
    }

    #[test]
    fn test_adapter_error_display() {
        assert_eq!(AdapterError::Timeout.to_string(), "agent request timed out");
        assert_eq!(
            AdapterError::Unavailable("down".to_string()).to_string(),
            "agent unavailable: down"
        );
        assert_eq!(
            AdapterError::Protocol("bad json".to_string()).to_string(),
            "protocol error: bad json"
        );
    }

    #[test]
    fn test_openclaw_uses_api_key_over_auth_token() {
        // api_key should take priority over auth_token
        let agent = AgentConfig {
            id: "test".to_string(),
            kind: "openclaw-http".to_string(),
            endpoint: "http://localhost".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: Some("old-token".to_string()),
            api_key: Some("new-api-key".to_string()),
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        // Should build without error — api_key takes priority
        let adapter = build_adapter(&agent).expect("should build");
        assert_eq!(adapter.kind(), "openclaw-http");
    }

    #[test]
    fn test_openclaw_accepts_api_key_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_file = dir.path().join("gateway-token");
        std::fs::write(&key_file, "file-token\n").expect("write token");
        let agent = AgentConfig {
            id: "gateway".to_string(),
            kind: "openclaw-http".to_string(),
            endpoint: "http://localhost".to_string(),
            timeout_ms: None,
            model: Some("local-kimi-gpt55".to_string()),
            auth_token: None,
            api_key: None,
            api_key_file: Some(key_file),
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };

        let adapter = build_adapter(&agent).expect("should build");
        assert_eq!(adapter.kind(), "openclaw-http");
    }

    // ── New adapter factory tests ────────────────────────────────────────────

    fn openclaw_native_agent() -> AgentConfig {
        AgentConfig {
            id: "test-librarian".to_string(),
            kind: "openclaw-native".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            timeout_ms: Some(5000),
            model: None,
            auth_token: None,
            api_key: Some("REPLACE_WITH_HOOKS_TOKEN".to_string()),
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    fn zeroclaw_native_agent() -> AgentConfig {
        AgentConfig {
            id: "test-zeroclaw".to_string(),
            kind: "zeroclaw-native".to_string(),
            endpoint: "http://127.0.0.1:18799".to_string(),
            timeout_ms: Some(5000),
            model: None,
            auth_token: Some("tok".to_string()),
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    #[test]
    fn test_build_openclaw_native_adapter() {
        let agent = openclaw_native_agent();
        let adapter = build_adapter(&agent).expect("should build openclaw-native adapter");
        assert_eq!(adapter.kind(), "openclaw-native");
    }

    #[test]
    fn test_build_zeroclaw_native_adapter() {
        let agent = zeroclaw_native_agent();
        let adapter = build_adapter(&agent).expect("should build zeroclaw-native adapter");
        assert_eq!(adapter.kind(), "zeroclaw-native");
    }

    #[test]
    fn test_openclaw_native_uses_api_key() {
        let agent = AgentConfig {
            id: "native-test".to_string(),
            kind: "openclaw-native".to_string(),
            endpoint: "http://localhost:18789".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: Some("old-token".to_string()),
            api_key: Some("new-hooks-token".to_string()),
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        // api_key takes precedence — should build without error
        let adapter = build_adapter(&agent).expect("should build");
        assert_eq!(adapter.kind(), "openclaw-native");
    }

    #[test]
    fn test_zeroclaw_native_uses_auth_token_fallback() {
        let agent = AgentConfig {
            id: "zeroclaw-test".to_string(),
            kind: "zeroclaw-native".to_string(),
            endpoint: "http://localhost:18799".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: Some("auth-token".to_string()),
            api_key: None, // no api_key — falls back to auth_token
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let adapter = build_adapter(&agent).expect("should build with auth_token fallback");
        assert_eq!(adapter.kind(), "zeroclaw-native");
    }

    #[test]
    fn test_openclaw_native_builds_without_token() {
        // Should build even with no token (empty string is valid — might be
        // an unauthenticated loopback deployment)
        let agent = AgentConfig {
            id: "no-token".to_string(),
            kind: "openclaw-native".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        let adapter = build_adapter(&agent).expect("should build with empty token");
        assert_eq!(adapter.kind(), "openclaw-native");
    }
}
