//! Router — dispatch messages to downstream agents via the adapter layer.
//!
//! The router selects the correct `AgentAdapter` for an agent's `kind`, then
//! calls `adapter.dispatch(text)`. All protocol details live in the adapter;
//! the router is purely a lookup + orchestration layer.

use anyhow::Result;
use tracing::{info, warn};

use crate::adapters::{
    agent_supports_model_override, agent_supports_native_commands, build_adapter, AdapterError,
    DispatchContext,
};
use crate::config::{AgentConfig, CalciforgeConfig};
use crate::context::is_native_agent_command;
use crate::messages::OutboundMessage;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// The agent router. Builds adapters on-demand from agent config.
pub struct Router;

/// Optional context collected by upstream channels before dispatch.
#[derive(Clone, Copy, Debug, Default)]
pub struct RouterDispatchContext<'a> {
    pub sender: Option<&'a str>,
    pub model_override: Option<&'a str>,
    pub session: Option<&'a str>,
    pub channel: Option<&'a str>,
}

impl Router {
    /// Create a new router.
    pub fn new() -> Self {
        Self
    }

    /// Dispatch a user message to the specified agent and return the text response.
    ///
    /// Selects the adapter based on `agent.kind` and calls `dispatch(text)`.
    #[cfg(test)]
    pub async fn dispatch(
        &self,
        text: &str,
        agent: &AgentConfig,
        config: &CalciforgeConfig,
    ) -> Result<String> {
        self.dispatch_with_sender(text, agent, config, None).await
    }

    /// Dispatch a message with optional sender identity forwarded to the agent.
    ///
    /// `sender` is the resolved Calciforge identity name (e.g. "brian").
    /// Forwarded to adapters that support per-sender context (`zeroclaw-http`).
    /// Other adapters ignore it.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "kept for callers that need sender context without model override"
        )
    )]
    pub async fn dispatch_with_sender(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &CalciforgeConfig,
        sender: Option<&str>,
    ) -> Result<String> {
        self.dispatch_with_sender_and_model(text, agent, _config, sender, None)
            .await
    }

    /// Dispatch a message with sender identity and optional model override.
    pub async fn dispatch_with_sender_and_model(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &CalciforgeConfig,
        sender: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<String> {
        let response = self
            .dispatch_message_with_sender_model_and_session(
                text,
                agent,
                _config,
                sender,
                model_override,
                None,
            )
            .await?;
        Ok(response.render_text_fallback())
    }

    /// Dispatch a message and preserve attachments/artifacts for channels that
    /// know how to render richer outbound messages.
    #[expect(
        dead_code,
        reason = "kept for callers that need sender/model context without downstream session selection"
    )]
    pub async fn dispatch_message_with_sender_and_model(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &CalciforgeConfig,
        sender: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<OutboundMessage> {
        self.dispatch_message_with_sender_model_and_session(
            text,
            agent,
            _config,
            sender,
            model_override,
            None,
        )
        .await
    }

    /// Dispatch a message with sender/model context and an optional downstream
    /// session selected by the authenticated user.
    pub async fn dispatch_message_with_sender_model_and_session(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &CalciforgeConfig,
        sender: Option<&str>,
        model_override: Option<&str>,
        session: Option<&str>,
    ) -> Result<OutboundMessage> {
        self.dispatch_message_with_full_context(
            text,
            agent,
            _config,
            RouterDispatchContext {
                sender,
                model_override,
                session,
                channel: None,
            },
        )
        .await
    }

    /// Dispatch a message with all currently supported upstream channel context.
    pub async fn dispatch_message_with_full_context(
        &self,
        text: &str,
        agent: &AgentConfig,
        _config: &CalciforgeConfig,
        context: RouterDispatchContext<'_>,
    ) -> Result<OutboundMessage> {
        let adapter = build_adapter(agent).map_err(|e| {
            anyhow::anyhow!("failed to build adapter for agent '{}': {}", agent.id, e)
        })?;

        let effective_model_override = if agent_supports_native_commands(agent)
            && is_native_agent_command(text)
        {
            None
        } else if agent_supports_model_override(agent) {
            context.model_override
        } else {
            if context.model_override.is_some() {
                info!(
                    agent_id = %agent.id,
                    kind = %agent.kind,
                    "active model override ignored because adapter kind does not consume Calciforge model overrides"
                );
            }
            None
        };

        info!(
            agent_id = %agent.id,
            kind = %agent.kind,
            endpoint = %agent.endpoint,
            configured_model = ?agent.model,
            model_override = ?effective_model_override,
            session = ?context.session,
            channel = ?context.channel,
            sender = ?context.sender,
            "routing message via {} adapter",
            adapter.kind()
        );

        let ctx = DispatchContext {
            message: text,
            sender: context.sender,
            model_override: effective_model_override,
            session: context.session,
            channel: context.channel,
        };
        adapter
            .dispatch_message_with_context(ctx)
            .await
            .map_err(|e| {
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
                        // extract the ZeroClawApprovalRequest for user notification.
                        return anyhow::Error::new(AdapterError::ApprovalPending(req.clone()));
                    }
                };
                anyhow::anyhow!("{}", msg)
            })
    }

    /// Dispatch a one-off prompt to a named configured agent without attaching
    /// downstream session state. Used by `!btw` so callers can ask another
    /// agent without changing their active agent or interrupting an active
    /// session thread.
    pub async fn dispatch_one_off_for_identity(
        &self,
        text: &str,
        agent_id: &str,
        config: &CalciforgeConfig,
        identity_id: &str,
        channel: &'static str,
        model_override: Option<&str>,
    ) -> Result<OutboundMessage> {
        let agent = config
            .agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{agent_id}' is not configured."))?;
        self.dispatch_message_with_full_context(
            text,
            agent,
            config,
            RouterDispatchContext {
                sender: Some(identity_id),
                model_override,
                session: None,
                channel: Some(channel),
            },
        )
        .await
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
    use crate::config::{AgentConfig, CalciforgeConfig, CalciforgeHeader};
    use std::collections::HashMap;

    fn base_config() -> CalciforgeConfig {
        CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![],
            agents: vec![],
            routing: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            security: None,
            proxy: None,
            local_models: None,
        }
    }

    fn openclaw_agent(endpoint: &str) -> AgentConfig {
        AgentConfig {
            id: "test-openclaw".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: endpoint.to_string(),
            timeout_ms: Some(500),
            model: None,
            auth_token: Some("test-token".to_string()),
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
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
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
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
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
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
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
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
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
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
