//! Router — dispatch messages to downstream agents via the adapter layer.
//!
//! The router selects the correct `AgentAdapter` for an agent's `kind`, then
//! calls `adapter.dispatch(text)`. All protocol details live in the adapter;
//! the router is purely a lookup + orchestration layer.

use anyhow::Result;
use tracing::{info, warn};

use crate::adapters::{build_adapter, AdapterError, DispatchContext};
use crate::config::{AgentConfig, PolyConfig};

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// The agent router. Builds adapters on-demand from agent config.
pub struct Router;

impl Router {
    /// Create a new router.
    pub fn new() -> Self {
        Self
    }

    /// Dispatch a user message to the specified agent and return the text response.
    ///
    /// Selects the adapter based on `agent.kind` and calls `dispatch(text)`.
    pub async fn dispatch(
        &self,
        text: &str,
        agent: &AgentConfig,
        config: &PolyConfig,
    ) -> Result<String> {
        self.dispatch_with_sender(text, agent, config, None).await
    }

    /// Dispatch a message with optional sender identity forwarded to the agent.
    ///
    /// `sender` is the resolved PolyClaw identity name (e.g. "brian").
    /// Forwarded to adapters that support per-sender context (`nzc-http`).
    /// Other adapters ignore it.
    pub async fn dispatch_with_sender(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &PolyConfig,
        sender: Option<&str>,
    ) -> Result<String> {
        let adapter = build_adapter(agent)
            .map_err(|e| anyhow::anyhow!("failed to build adapter for agent '{}': {}", agent.id, e))?;

        info!(
            agent_id = %agent.id,
            kind = %agent.kind,
            sender = ?sender,
            "routing message via {} adapter",
            adapter.kind()
        );

        let ctx = DispatchContext { message: text, sender };
        adapter.dispatch_with_context(ctx).await.map_err(|e| {
            let msg = match &e {
                AdapterError::Timeout => format!("agent '{}' timed out", agent.id),
                AdapterError::Unavailable(s) => {
                    warn!(agent_id = %agent.id, detail = %s, "agent unavailable");
                    format!("agent '{}' unavailable: {}", agent.id, s)
                }
                AdapterError::Protocol(s) => {
                    warn!(agent_id = %agent.id, detail = %s, "agent protocol error");
                    format!("agent '{}' protocol error: {}", agent.id, s)
                }
                AdapterError::ApprovalPending(req) => {
                    // Re-wrap as anyhow error so callers can downcast and
                    // extract the NzcApprovalRequest for user notification.
                    return anyhow::Error::new(
                        AdapterError::ApprovalPending(req.clone())
                    );
                }
            };
            anyhow::anyhow!("{}", msg)
        })
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, PolyConfig, PolyHeader};
    use std::collections::HashMap;

    fn base_config() -> PolyConfig {
        PolyConfig {
            polyclaw: PolyHeader { version: 2 },
            identities: vec![],
            agents: vec![],
            routing: vec![],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
        }
    }

    fn openclaw_agent(endpoint: &str) -> AgentConfig {
        AgentConfig {
            id: "test-openclaw".to_string(),
            kind: "openclaw-http".to_string(),
            endpoint: endpoint.to_string(),
            timeout_ms: Some(500),
            model: None,
            auth_token: Some("test-token".to_string()),
            api_key: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    fn zeroclaw_agent(endpoint: &str) -> AgentConfig {
        AgentConfig {
            id: "test-zeroclaw".to_string(),
            kind: "zeroclaw".to_string(),
            endpoint: endpoint.to_string(),
            timeout_ms: Some(500),
            model: None,
            auth_token: None,
            api_key: Some("zc_test".to_string()),
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        }
    }

    fn cli_echo_agent() -> AgentConfig {
        AgentConfig {
            id: "test-cli".to_string(),
            kind: "cli".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(5000),
            model: None,
            auth_token: None,
            api_key: None,
            command: Some("/bin/echo".to_string()),
            args: Some(vec!["{message}".to_string()]),
            env: Some(HashMap::new()),
            registry: None,
            aliases: vec![],
        }
    }

    #[test]
    fn test_router_creates() {
        let _r = Router::new();
    }

    #[test]
    fn test_unknown_kind_returns_error() {
        let agent = AgentConfig {
            id: "bad".to_string(),
            kind: "not-real".to_string(),
            endpoint: "http://localhost".to_string(),
            timeout_ms: None,
            model: None,
            auth_token: None,
            api_key: None,
            command: None,
            args: None,
            env: None,
            registry: None,
            aliases: vec![],
        };
        // build_adapter is synchronous — test it directly
        let result = build_adapter(&agent);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_openclaw_unreachable() {
        let router = Router::new();
        let agent = openclaw_agent("http://127.0.0.1:19093");
        let cfg = base_config();
        let result = router.dispatch("ping", &agent, &cfg).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_zeroclaw_unreachable() {
        let router = Router::new();
        let agent = zeroclaw_agent("http://127.0.0.1:19094");
        let cfg = base_config();
        let result = router.dispatch("ping", &agent, &cfg).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_cli_echo() {
        let router = Router::new();
        let agent = cli_echo_agent();
        let cfg = base_config();
        let result = router.dispatch("hello-router", &agent, &cfg).await;
        assert!(result.is_ok(), "echo should work: {:?}", result);
        assert_eq!(result.unwrap(), "hello-router");
    }

    #[tokio::test]
    async fn test_dispatch_cli_bad_binary() {
        let router = Router::new();
        let agent = AgentConfig {
            id: "bad-cli".to_string(),
            kind: "cli".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(500),
            model: None,
            auth_token: None,
            api_key: None,
            command: Some("/nonexistent/bin/xyzzy".to_string()),
            args: None,
            env: Some(HashMap::new()),
            registry: None,
            aliases: vec![],
        };
        let cfg = base_config();
        let result = router.dispatch("ping", &agent, &cfg).await;
        assert!(result.is_err());
    }
}
