//! Local command handler for PolyClaw v2.
//!
//! Commands starting with `!` are handled locally — they never reach the agent.
//! All other messages route to the agent as normal.
//!
//! # Command routing
//!
//! Some commands (`!help`, `!agents`, `!metrics`, `!ping`) require no
//! auth context and are intercepted before identity resolution.
//!
//! Other commands (`!switch`, `!status`) require an authenticated identity and
//! are handled after auth via [`CommandHandler::handle_switch`] and
//! [`CommandHandler::cmd_status_for_identity`] respectively.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::Instant;
use std::path::PathBuf;

use crate::adapters::openclaw::{NzcHttpAdapter, SharedPendingApprovals};
use crate::config::PolyConfig;

/// Path to the persisted active-agent state file.
fn state_file_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".polyclaw").join("state").join("active-agents.json")
}

/// Load persisted active-agent selections from disk.
/// Returns an empty map if the file doesn't exist or can't be parsed.
fn load_active_agents() -> HashMap<String, String> {
    let path = state_file_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Persist the active-agent map to disk so it survives restarts.
fn save_active_agents(map: &HashMap<String, String>) {
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(&path, json);
    }
}

/// In-memory command handler with simple counters and per-identity active-agent state.
pub struct CommandHandler {
    start_time: Instant,
    config: Arc<PolyConfig>,
    messages_routed: AtomicU64,
    total_latency_ms: AtomicU64,
    /// Per-identity active agent: identity_id → agent_id.
    /// Persisted to ~/.polyclaw/state/active-agents.json and loaded on startup.
    active_agents: Mutex<HashMap<String, String>>,
    /// Pending Clash approvals: request_id → NZC endpoint + metadata.
    /// Shared with any `NzcHttpAdapter` instances created for the same agent
    /// so that `!approve` / `!deny` can signal the right NZC instance.
    pub pending_approvals: SharedPendingApprovals,
    /// reqwest client reused for approve/deny HTTP calls.
    http_client: reqwest::Client,
}

impl CommandHandler {
    /// Create a new CommandHandler, loading any persisted agent selections from disk.
    pub fn new(config: Arc<PolyConfig>) -> Self {
        let active_agents = load_active_agents();
        if !active_agents.is_empty() {
            tracing::info!(
                agents = ?active_agents,
                "loaded persisted active-agent selections"
            );
        }
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client for command handler");
        Self {
            start_time: Instant::now(),
            config,
            messages_routed: AtomicU64::new(0),
            total_latency_ms: AtomicU64::new(0),
            active_agents: Mutex::new(active_agents),
            pending_approvals: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            http_client,
        }
    }

    /// Record that a message was routed to an agent.
    ///
    /// Call this after a successful agent dispatch with the measured latency.
    pub fn record_dispatch(&self, latency_ms: u64) {
        self.messages_routed.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
    }

    /// Return the currently active agent ID for the given identity.
    ///
    /// Falls back to `default_agent` from the routing config if no explicit switch
    /// has been made.  Returns `None` if the identity has no routing rule.
    pub fn active_agent_for(&self, identity_id: &str) -> Option<String> {
        // Check the in-memory override first.
        {
            let map = self.active_agents.lock().unwrap();
            if let Some(agent) = map.get(identity_id) {
                return Some(agent.clone());
            }
        }
        // Fall back to the config default.
        crate::auth::default_agent_for(identity_id, &self.config)
    }

    /// Handle a pre-auth command (commands that do not require identity context).
    ///
    /// Returns `Some(response)` if `text` starts with `!` and matches a known
    /// pre-auth command.  Returns `None` otherwise (caller should proceed with
    /// auth and routing).
    ///
    /// **Note:** `!switch` and `!status` are intentionally NOT handled here —
    /// they need identity context and are handled after auth via [`handle_switch`]
    /// and [`cmd_status_for_identity`] respectively.
    pub fn handle(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if !trimmed.starts_with('!') {
            return None;
        }

        // Grab just the command word (before any args)
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();

        match cmd.as_str() {
            "!help"    => Some(self.cmd_help()),
            // !status needs auth — return None so the caller resolves identity first.
            "!status"  => None,
            "!agents"  => Some(self.cmd_agents()),
            "!metrics" => Some(self.cmd_metrics()),
            "!ping"    => Some("pong".to_string()),
            // !switch needs auth — return None here so the caller can do auth
            // first, then call handle_switch().
            "!switch"  => None,
            // !default needs auth — switches back to the configured default agent.
            "!default" => None,
            _ => None, // Unknown !command — fall through to agent
        }
    }

    /// Returns `true` if the text is a `!switch` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`handle_switch`] instead of
    /// routing to the agent.
    pub fn is_switch_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();
        cmd == "!switch"
    }

    /// Returns `true` if the text is a `!default` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`handle_default`] instead of
    /// routing to the agent.
    pub fn is_default_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();
        cmd == "!default"
    }

    /// Returns `true` if the text is a `!status` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`cmd_status_for_identity`]
    /// instead of routing to the agent.
    pub fn is_status_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();
        cmd == "!status"
    }

    /// Returns `true` if the text is an `!approve` command (case-insensitive).
    pub fn is_approve_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();
        cmd == "!approve"
    }

    /// Returns `true` if the text is a `!deny` command (case-insensitive).
    pub fn is_deny_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = trimmed.splitn(2, ' ').next().unwrap_or("").to_lowercase();
        cmd == "!deny"
    }

    /// Handle a command that may require async work (approve/deny).
    ///
    /// Returns `Some((ack, Option<follow_up>))` if the text matches `!approve`
    /// or `!deny`, `None` if it is not a recognized async command.
    ///
    /// Callers should send `ack` immediately, then send `follow_up` (if present)
    /// once it arrives — it carries the continuation agent response after the
    /// approval/denial has been relayed to NZC and polled for a result.
    pub async fn handle_async(&self, text: &str) -> Option<(String, Option<String>)> {
        if Self::is_approve_command(text) {
            let (ack, follow_up) = self.handle_approve(text).await;
            Some((ack, follow_up))
        } else if Self::is_deny_command(text) {
            let (ack, follow_up) = self.handle_deny(text).await;
            Some((ack, follow_up))
        } else {
            None
        }
    }

    /// Register a pending approval for later `!approve` / `!deny` handling.
    ///
    /// Called by the channel dispatcher when it receives an `ApprovalPending`
    /// error from the router.
    pub async fn register_pending_approval(
        &self,
        meta: crate::adapters::openclaw::PendingApprovalMeta,
    ) {
        self.pending_approvals
            .lock()
            .await
            .insert(meta.request_id.clone(), meta);
    }

    /// Handle an `!approve [request_id]` command.
    ///
    /// If no `request_id` is provided and exactly one approval is pending,
    /// auto-selects it.  Signals NZC to allow the blocked tool call, then
    /// polls for the continuation result (up to 10 minutes).
    ///
    /// Returns `(reply_message, Option<final_agent_response>)`.
    pub async fn handle_approve(&self, text: &str) -> (String, Option<String>) {
        let args = text.trim().splitn(3, ' ').collect::<Vec<_>>();
        // args[0] = "!approve", args[1] = optional request_id
        let explicit_id = args.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());

        let meta = self.resolve_pending_approval(explicit_id).await;
        let meta = match meta {
            Ok(m) => m,
            Err(msg) => return (msg, None),
        };

        // Signal NZC to approve.
        match NzcHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.nzc_endpoint,
            &meta.nzc_auth_token,
            &meta.request_id,
            true,
            None,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (
                    format!("⚠️ Failed to send approval: {e}"),
                    None,
                );
            }
        }

        // Remove from local pending store.
        self.pending_approvals.lock().await.remove(&meta.request_id);

        // Poll for the continuation result.
        let result = NzcHttpAdapter::poll_result(
            &self.http_client,
            &meta.nzc_endpoint,
            &meta.nzc_auth_token,
            &meta.request_id,
        )
        .await;

        match result {
            Ok(response) => (
                format!("✅ Approved (request {})", meta.request_id),
                Some(response),
            ),
            Err(e) => (
                format!("✅ Approved — but failed to retrieve result: {e}"),
                None,
            ),
        }
    }

    /// Handle a `!deny [request_id] [reason]` command.
    ///
    /// If no `request_id` is provided and exactly one approval is pending,
    /// auto-selects it.  Signals NZC to deny the blocked tool call, then
    /// polls for the continuation result.
    pub async fn handle_deny(&self, text: &str) -> (String, Option<String>) {
        let trimmed = text.trim();
        // Parse: "!deny [request_id] [reason...]"
        let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
        let (explicit_id, reason) = match parts.len() {
            1 => (None, None),
            2 => (Some(parts[1].trim()), None),
            _ => {
                // Try to distinguish: if parts[1] looks like a UUID, treat as id+reason.
                // Otherwise treat the whole tail as a reason with no explicit id.
                let candidate = parts[1].trim();
                if candidate.len() == 36 && candidate.contains('-') {
                    (Some(candidate), Some(parts[2].trim()))
                } else {
                    (None, Some(&trimmed[6..])) // skip "!deny "
                }
            }
        };

        let meta = self.resolve_pending_approval(explicit_id).await;
        let meta = match meta {
            Ok(m) => m,
            Err(msg) => return (msg, None),
        };

        // Signal NZC to deny.
        match NzcHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.nzc_endpoint,
            &meta.nzc_auth_token,
            &meta.request_id,
            false,
            reason,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (
                    format!("⚠️ Failed to send denial: {e}"),
                    None,
                );
            }
        }

        // Remove from local pending store.
        self.pending_approvals.lock().await.remove(&meta.request_id);

        // Poll for the continuation result.
        let result = NzcHttpAdapter::poll_result(
            &self.http_client,
            &meta.nzc_endpoint,
            &meta.nzc_auth_token,
            &meta.request_id,
        )
        .await;

        match result {
            Ok(response) => (
                format!("🚫 Denied (request {})", meta.request_id),
                Some(response),
            ),
            Err(e) => (
                format!("🚫 Denied — but failed to retrieve result: {e}"),
                None,
            ),
        }
    }

    /// Resolve the pending approval to act on.
    ///
    /// If `explicit_id` is `Some`, looks up by that ID.
    /// If `None`, auto-selects the single pending approval (or errors if 0 or >1).
    async fn resolve_pending_approval(
        &self,
        explicit_id: Option<&str>,
    ) -> Result<crate::adapters::openclaw::PendingApprovalMeta, String> {
        let store = self.pending_approvals.lock().await;
        if let Some(id) = explicit_id {
            match store.get(id) {
                Some(meta) => Ok(meta.clone()),
                None => Err(format!(
                    "⚠️ No pending approval with ID '{id}'.\n\nUse !approve or !deny without an ID to list pending approvals."
                )),
            }
        } else {
            match store.len() {
                0 => Err("⚠️ No pending approvals.".to_string()),
                1 => Ok(store.values().next().unwrap().clone()),
                n => {
                    let ids: Vec<&str> = store.keys().map(|s| s.as_str()).collect();
                    Err(format!(
                        "⚠️ {n} pending approvals. Specify a request ID:\n{}",
                        ids.join("\n")
                    ))
                }
            }
        }
    }

    /// Return a status string for the given authenticated identity.
    ///
    /// Uses [`active_agent_for`] to show the per-identity active agent rather
    /// than blindly reading the first routing rule's `default_agent`.
    pub fn cmd_status_for_identity(&self, identity_id: &str) -> String {
        let uptime = self.start_time.elapsed();
        let uptime_secs = uptime.as_secs();
        let hours = uptime_secs / 3600;
        let minutes = (uptime_secs % 3600) / 60;
        let seconds = uptime_secs % 60;

        let version = self.config.polyclaw.version;
        let agent_count = self.config.agents.len();
        let identity_count = self.config.identities.len();
        let channel_count = self.config.channels.len();

        // Use the real per-identity active agent (respects !switch overrides).
        let active_agent = self
            .active_agent_for(identity_id)
            .unwrap_or_else(|| "none".to_string());

        // Get model/provider info for the active agent
        let model_info = self.config.agents.iter()
            .find(|a| a.id == active_agent)
            .map(|agent| {
                let model = agent.model.as_deref().unwrap_or("default");
                let provider = &agent.kind;
                if provider.contains("alloy") || model.contains("alloy") {
                    format!("\n  provider: {provider} (alloy)\n  model: {model}")
                } else {
                    format!("\n  provider: {provider}\n  model: {model}")
                }
            })
            .unwrap_or_default();

        format!(
            "PolyClaw v2 status:\n  version: {version}\n  uptime: {hours}h {minutes}m {seconds}s\n  active agent: {active_agent}{model_info}\n  agents: {agent_count}, identities: {identity_count}, channels: {channel_count}"
        )
    }

    /// Handle a `!switch <agent>` command for an authenticated identity.
    ///
    /// Validates the requested agent against the identity's `allowed_agents`,
    /// updates the active-agent map, and returns a confirmation message.
    ///
    /// Returns an error string (to be sent back to the user) on any validation
    /// failure — never panics.
    pub fn handle_switch(&self, text: &str, identity_id: &str) -> String {
        let trimmed = text.trim();
        // Parse the agent argument (everything after "!switch ")
        let agent_arg = trimmed
            .splitn(2, ' ')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string();

        if agent_arg.is_empty() {
            return format!(
                "Usage: !switch <agent>\n\nUse !agents to see available agents."
            );
        }

        // Look up the routing rule for this identity.
        let routing_rule = match self.config.routing.iter().find(|r| r.identity == identity_id) {
            Some(r) => r,
            None => {
                return "⚠️ No routing rule found for your identity.".to_string();
            }
        };

        // Determine which agents this identity is allowed to switch to.
        // Empty allowed_agents means unrestricted (any configured agent).
        let allowed: Vec<&str> = if routing_rule.allowed_agents.is_empty() {
            self.config.agents.iter().map(|a| a.id.as_str()).collect()
        } else {
            routing_rule.allowed_agents.iter().map(|s| s.as_str()).collect()
        };

        // Case-insensitive match of the requested agent against allowed list,
        // checking both agent id and any configured aliases.
        let matched_agent = allowed.iter().find(|&&a| {
            // Direct id match
            if a.eq_ignore_ascii_case(&agent_arg) {
                return true;
            }
            // Alias match — look up the agent and check its aliases
            if let Some(agent_cfg) = self.config.agents.iter().find(|ag| ag.id == a) {
                return agent_cfg
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&agent_arg));
            }
            false
        }).copied();

        match matched_agent {
            None => {
                // Build a helpful rejection message listing valid options.
                let valid = allowed.join(", ");
                format!(
                    "⚠️ Agent '{}' is not available to you.\n\nValid agents: {}",
                    agent_arg, valid
                )
            }
            Some(agent_id) => {
                // Look up display name from registry metadata (if any).
                let display_name = self
                    .config
                    .agents
                    .iter()
                    .find(|a| a.id == agent_id)
                    .and_then(|a| a.registry.as_ref())
                    .and_then(|r| r.display_name.as_deref())
                    .unwrap_or(agent_id);

                // Update per-identity active agent and persist to disk.
                {
                    let mut map = self.active_agents.lock().unwrap();
                    map.insert(identity_id.to_string(), agent_id.to_string());
                    save_active_agents(&map);
                }

                format!(
                    "✅ Switched to {}. Your messages will now route to {}.",
                    display_name, agent_id
                )
            }
        }
    }

    /// Handle a `!default` command for an authenticated identity.
    ///
    /// Looks up the identity's configured `default_agent` from the routing table
    /// and switches the in-memory active agent back to it.
    ///
    /// Returns a confirmation message or an error string if no routing rule exists.
    pub fn handle_default(&self, identity_id: &str) -> String {
        let default_agent_id = match crate::auth::default_agent_for(identity_id, &self.config) {
            Some(id) => id,
            None => return "⚠️ No routing rule found for your identity.".to_string(),
        };

        // Update per-identity active agent back to the configured default and persist.
        {
            let mut map = self.active_agents.lock().unwrap();
            map.insert(identity_id.to_string(), default_agent_id.clone());
            save_active_agents(&map);
        }

        format!("✅ Switched to default agent: {}", default_agent_id)
    }

    // -----------------------------------------------------------------------
    // Individual command handlers
    // -----------------------------------------------------------------------

    fn cmd_help(&self) -> String {
        [
            "PolyClaw v2 — available commands:",
            "  !help    — show this help",
            "  !status  — version, uptime, active agent, config summary",
            "  !agents  — list configured agents with endpoints",
            "  !metrics — messages routed, average latency",
            "  !ping    — connectivity check (replies: pong)",
            "  !switch <agent> — switch active agent (requires auth)",
            "  !default — switch back to your default agent (requires auth)",
            "  !approve [request_id] — approve a pending Clash tool call",
            "  !deny [request_id] [reason] — deny a pending Clash tool call",
        ]
        .join("\n")
    }

    /// Fallback status without identity context.
    ///
    /// **Deprecated in favour of [`cmd_status_for_identity`]** which uses the
    /// per-identity active agent and correctly reflects `!switch` overrides.
    /// Kept for test backward-compatibility only — not called from the live
    /// Telegram dispatcher.
    #[cfg(test)]
    fn cmd_status(&self) -> String {
        let uptime = self.start_time.elapsed();
        let uptime_secs = uptime.as_secs();
        let hours = uptime_secs / 3600;
        let minutes = (uptime_secs % 3600) / 60;
        let seconds = uptime_secs % 60;

        let version = self.config.polyclaw.version;
        let agent_count = self.config.agents.len();
        let identity_count = self.config.identities.len();
        let channel_count = self.config.channels.len();

        // Default agent — first routing rule's default, or "none"
        let default_agent = self
            .config
            .routing
            .first()
            .map(|r| r.default_agent.as_str())
            .unwrap_or("none");

        // Get model/provider info for the default agent
        let model_info = self.config.agents.iter()
            .find(|a| a.id == default_agent)
            .map(|agent| {
                let model = agent.model.as_deref().unwrap_or("default");
                let provider = &agent.kind;
                if provider.contains("alloy") || model.contains("alloy") {
                    format!("\n  provider: {provider} (alloy)\n  model: {model}")
                } else {
                    format!("\n  provider: {provider}\n  model: {model}")
                }
            })
            .unwrap_or_default();

        format!(
            "PolyClaw v2 status:\n  version: {version}\n  uptime: {hours}h {minutes}m {seconds}s\n  active agent: {default_agent}{model_info}\n  agents: {agent_count}, identities: {identity_count}, channels: {channel_count}"
        )
    }

    fn cmd_agents(&self) -> String {
        if self.config.agents.is_empty() {
            return "No agents configured.".to_string();
        }

        let mut lines = vec!["Configured agents:".to_string()];
        for agent in &self.config.agents {
            // For CLI agents, show command instead of endpoint
            let location = if agent.kind == "cli" {
                agent.command.as_deref().unwrap_or("(no command)").to_string()
            } else {
                agent.endpoint.clone()
            };
            lines.push(format!(
                "  {} ({}) — {}",
                agent.id, agent.kind, location
            ));
        }
        lines.join("\n")
    }

    fn cmd_metrics(&self) -> String {
        let routed = self.messages_routed.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ms.load(Ordering::Relaxed);
        let avg_latency = if routed > 0 {
            total_latency / routed
        } else {
            0
        };

        format!(
            "PolyClaw v2 metrics:\n  messages routed: {routed}\n  avg latency: {avg_latency}ms"
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, AgentRegistry, ChannelAlias, ChannelConfig, Identity, PolyConfig, PolyHeader,
        RoutingRule,
    };

    fn make_handler() -> CommandHandler {
        let config = Arc::new(make_config());
        CommandHandler::new(config)
    }

    fn make_config() -> PolyConfig {
        PolyConfig {
            polyclaw: PolyHeader { version: 2 },
            identities: vec![
                Identity {
                    id: "brian".to_string(),
                    display_name: Some("Brian".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "8465871195".to_string(),
                    }],
                    role: Some("owner".to_string()),
                },
                Identity {
                    id: "david".to_string(),
                    display_name: Some("David".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "15555550002".to_string(),
                    }],
                    role: Some("user".to_string()),
                },
            ],
            agents: vec![
                AgentConfig {
                    id: "librarian".to_string(),
                    kind: "openclaw-http".to_string(),
                    endpoint: "http://10.0.0.20:18789".to_string(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
                    api_key: None,
                    command: None,
                    args: None,
                    env: None,
                    registry: Some(AgentRegistry {
                        display_name: Some("Librarian".to_string()),
                        ..Default::default()
                    }),
                    aliases: vec![],
                },
                AgentConfig {
                    id: "custodian".to_string(),
                    kind: "openclaw-http".to_string(),
                    endpoint: "http://10.0.0.50:18789".to_string(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
                    api_key: None,
                    command: None,
                    args: None,
                    env: None,
                    registry: None,
                    aliases: vec!["keeper".to_string(), "cust".to_string()],
                },
            ],
            routing: vec![
                RoutingRule {
                    identity: "brian".to_string(),
                    default_agent: "librarian".to_string(),
                    allowed_agents: vec![], // unrestricted
                },
                RoutingRule {
                    identity: "david".to_string(),
                    default_agent: "librarian".to_string(),
                    allowed_agents: vec!["librarian".to_string()], // restricted
                },
            ],
            channels: vec![ChannelConfig {
                kind: "telegram".to_string(),
                bot_token_file: Some("~/.polyclaw/secrets/telegram-token".to_string()),
                enabled: true,
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
        }
    }

    // --- Basic command dispatch ---

    #[test]
    fn test_ping_returns_pong() {
        let h = make_handler();
        assert_eq!(h.handle("!ping"), Some("pong".to_string()));
    }

    #[test]
    fn test_ping_with_whitespace() {
        let h = make_handler();
        assert_eq!(h.handle("  !ping  "), Some("pong".to_string()));
    }

    #[test]
    fn test_non_command_returns_none() {
        let h = make_handler();
        assert!(h.handle("hello world").is_none());
        assert!(h.handle("what time is it?").is_none());
        assert!(h.handle("").is_none());
    }

    #[test]
    fn test_unknown_bang_command_returns_none() {
        let h = make_handler();
        // Unknown !commands fall through to agent
        assert!(h.handle("!unknown").is_none());
        assert!(h.handle("!foo bar").is_none());
    }

    // --- !help ---

    #[test]
    fn test_help_contains_all_commands() {
        let h = make_handler();
        let reply = h.handle("!help").unwrap();
        assert!(reply.contains("!help"));
        assert!(reply.contains("!status"));
        assert!(reply.contains("!agents"));
        assert!(reply.contains("!metrics"));
        assert!(reply.contains("!ping"));
        assert!(reply.contains("!switch"));
    }

    // --- !status ---

    #[test]
    fn test_status_handle_returns_none_pre_auth() {
        // !status must NOT be handled pre-auth — it needs identity context
        let h = make_handler();
        assert!(h.handle("!status").is_none(), "!status must return None from handle()");
        assert!(h.handle("!STATUS").is_none());
        assert!(h.handle("!Status").is_none());
    }

    #[test]
    fn test_is_status_command_detection() {
        assert!(CommandHandler::is_status_command("!status"));
        assert!(CommandHandler::is_status_command("  !STATUS  "));
        assert!(CommandHandler::is_status_command("!Status"));
        assert!(!CommandHandler::is_status_command("!ping"));
        assert!(!CommandHandler::is_status_command("!switch foo"));
        assert!(!CommandHandler::is_status_command("status")); // no !
    }

    #[test]
    fn test_status_contains_version() {
        let h = make_handler();
        let reply = h.cmd_status_for_identity("brian");
        assert!(reply.contains("version: 2"), "should show version 2");
    }

    #[test]
    fn test_status_contains_active_agent() {
        let h = make_handler();
        // Default (no switch): should show librarian
        let reply = h.cmd_status_for_identity("brian");
        assert!(reply.contains("librarian"), "should show active agent 'librarian'");
    }

    #[test]
    fn test_status_reflects_switch() {
        let h = make_handler();
        // Switch brian to custodian
        h.handle_switch("!switch custodian", "brian");
        let reply = h.cmd_status_for_identity("brian");
        assert!(reply.contains("custodian"), "status should reflect !switch: {}", reply);
        assert!(!reply.contains("librarian") || reply.contains("custodian"),
                "status should show switched agent: {}", reply);
    }

    #[test]
    fn test_status_independent_per_identity() {
        let h = make_handler();
        h.handle_switch("!switch custodian", "brian");
        // brian switched to custodian — david should still see librarian
        let brian_reply = h.cmd_status_for_identity("brian");
        let david_reply = h.cmd_status_for_identity("david");
        assert!(brian_reply.contains("custodian"), "brian should see custodian: {}", brian_reply);
        assert!(david_reply.contains("librarian"), "david should still see librarian: {}", david_reply);
    }

    #[test]
    fn test_status_contains_uptime() {
        let h = make_handler();
        let reply = h.cmd_status_for_identity("brian");
        assert!(reply.contains("uptime:"), "should contain uptime");
    }

    // --- !agents ---

    #[test]
    fn test_agents_lists_configured_agents() {
        let h = make_handler();
        let reply = h.handle("!agents").unwrap();
        assert!(reply.contains("librarian"), "should show agent id");
        assert!(reply.contains("10.0.0.20"), "should show endpoint");
        assert!(reply.contains("openclaw-http"), "should show agent kind");
    }

    #[test]
    fn test_agents_empty_config() {
        let config = Arc::new(PolyConfig {
            polyclaw: PolyHeader { version: 2 },
            identities: vec![],
            agents: vec![],
            routing: vec![],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
        });
        let h = CommandHandler::new(config);
        let reply = h.handle("!agents").unwrap();
        assert!(reply.contains("No agents"));
    }

    // --- !metrics ---

    #[test]
    fn test_metrics_initial_zero() {
        let h = make_handler();
        let reply = h.handle("!metrics").unwrap();
        assert!(reply.contains("messages routed: 0"));
        assert!(reply.contains("avg latency: 0ms"));
    }

    #[test]
    fn test_metrics_after_dispatches() {
        let h = make_handler();
        h.record_dispatch(100);
        h.record_dispatch(200);
        h.record_dispatch(300);

        let reply = h.handle("!metrics").unwrap();
        assert!(reply.contains("messages routed: 3"));
        assert!(reply.contains("avg latency: 200ms")); // (100+200+300)/3
    }

    // --- case insensitivity ---

    #[test]
    fn test_commands_case_insensitive() {
        let h = make_handler();
        assert_eq!(h.handle("!PING"), Some("pong".to_string()));
        assert_eq!(h.handle("!Ping"), Some("pong".to_string()));
        assert!(h.handle("!HELP").is_some());
        // !STATUS now requires identity context — returns None from handle()
        assert!(h.handle("!STATUS").is_none());
        // cmd_status_for_identity is case-insensitive at the identity level
        assert!(h.cmd_status_for_identity("brian").contains("version:"));
    }

    // --- record_dispatch counter ---

    #[test]
    fn test_record_dispatch_increments_counter() {
        let h = make_handler();
        assert_eq!(h.messages_routed.load(Ordering::Relaxed), 0);
        h.record_dispatch(50);
        assert_eq!(h.messages_routed.load(Ordering::Relaxed), 1);
        h.record_dispatch(150);
        assert_eq!(h.messages_routed.load(Ordering::Relaxed), 2);
    }

    // -----------------------------------------------------------------------
    // !switch tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_switch_is_not_handled_pre_auth() {
        // !switch must return None from handle() — it needs identity context
        let h = make_handler();
        assert!(h.handle("!switch custodian").is_none());
        assert!(h.handle("!SWITCH custodian").is_none());
    }

    #[test]
    fn test_is_switch_command_detection() {
        assert!(CommandHandler::is_switch_command("!switch custodian"));
        assert!(CommandHandler::is_switch_command("  !SWITCH custodian  "));
        assert!(CommandHandler::is_switch_command("!Switch librarian"));
        assert!(!CommandHandler::is_switch_command("!ping"));
        assert!(!CommandHandler::is_switch_command("!help"));
        assert!(!CommandHandler::is_switch_command("switch custodian")); // no !
        assert!(!CommandHandler::is_switch_command("hello world"));
    }

    #[test]
    fn test_switch_updates_active_agent_for_identity() {
        let h = make_handler();
        // Default is librarian
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));

        // Switch to custodian
        let reply = h.handle_switch("!switch custodian", "brian");
        assert!(reply.contains("custodian"), "reply should mention the agent: {}", reply);
        assert!(reply.contains('✅'), "should be a success reply: {}", reply);

        // Active agent is now custodian
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_updates_routing_for_subsequent_messages() {
        let h = make_handler();
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));

        h.handle_switch("!switch custodian", "brian");
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));

        // Switching back also works
        h.handle_switch("!switch librarian", "brian");
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
    }

    #[test]
    fn test_switch_rejects_disallowed_agent_for_restricted_identity() {
        let h = make_handler();
        // david is restricted to allowed_agents = ["librarian"]
        let reply = h.handle_switch("!switch custodian", "david");
        assert!(reply.contains("⚠️"), "should be a rejection: {}", reply);
        assert!(reply.contains("custodian"), "should mention the rejected agent: {}", reply);
        // Active agent should NOT have changed
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    #[test]
    fn test_switch_rejects_unknown_agent_with_valid_options() {
        let h = make_handler();
        let reply = h.handle_switch("!switch nonexistent", "brian");
        assert!(reply.contains("⚠️"), "should be a rejection: {}", reply);
        assert!(reply.contains("nonexistent"), "should mention the requested agent: {}", reply);
        // Should list valid agents
        assert!(
            reply.contains("librarian") || reply.contains("custodian"),
            "should list valid agents: {}",
            reply
        );
    }

    #[test]
    fn test_switch_without_agent_arg_returns_usage() {
        let h = make_handler();
        let reply = h.handle_switch("!switch", "brian");
        assert!(reply.to_lowercase().contains("usage") || reply.contains("!switch"), 
                "should show usage: {}", reply);
    }

    #[test]
    fn test_switch_case_insensitive_agent_name() {
        let h = make_handler();
        // "CUSTODIAN" should match "custodian"
        let reply = h.handle_switch("!switch CUSTODIAN", "brian");
        assert!(reply.contains('✅'), "case-insensitive switch should succeed: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_shows_display_name_in_reply() {
        let h = make_handler();
        // librarian has display_name = "Librarian" in registry
        let reply = h.handle_switch("!switch librarian", "brian");
        assert!(reply.contains("Librarian"), "should show display name: {}", reply);
    }

    #[test]
    fn test_switch_no_routing_rule_for_identity() {
        let h = make_handler();
        let reply = h.handle_switch("!switch librarian", "unknown_identity");
        assert!(reply.contains("⚠️"), "should reject unknown identity: {}", reply);
    }

    #[test]
    fn test_active_agent_defaults_to_config_default() {
        let h = make_handler();
        // No switch performed — should return config default
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    #[test]
    fn test_active_agent_unknown_identity_returns_none() {
        let h = make_handler();
        assert!(h.active_agent_for("stranger").is_none());
    }

    #[test]
    fn test_switch_independent_per_identity() {
        let h = make_handler();
        // Switch brian to custodian, david should be unaffected
        h.handle_switch("!switch custodian", "brian");
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    // -----------------------------------------------------------------------
    // Agent alias tests (!switch <alias>)
    // -----------------------------------------------------------------------

    #[test]
    fn test_switch_by_alias_succeeds() {
        let h = make_handler();
        // "keeper" is an alias for custodian
        let reply = h.handle_switch("!switch keeper", "brian");
        assert!(reply.contains('✅'), "alias switch should succeed: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_by_alias_case_insensitive() {
        let h = make_handler();
        let reply = h.handle_switch("!switch CUST", "brian");
        assert!(reply.contains('✅'), "case-insensitive alias switch should succeed: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_alias_not_in_allowed_is_rejected() {
        let h = make_handler();
        // david is restricted to allowed_agents = ["librarian"]; "keeper" is custodian alias
        let reply = h.handle_switch("!switch keeper", "david");
        assert!(reply.contains("⚠️"), "alias outside allowed list must be rejected: {}", reply);
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    // -----------------------------------------------------------------------
    // !default command tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_command_not_handled_pre_auth() {
        let h = make_handler();
        assert!(h.handle("!default").is_none(), "!default must return None from handle()");
        assert!(h.handle("!DEFAULT").is_none());
    }

    #[test]
    fn test_is_default_command_detection() {
        assert!(CommandHandler::is_default_command("!default"));
        assert!(CommandHandler::is_default_command("  !DEFAULT  "));
        assert!(CommandHandler::is_default_command("!Default"));
        assert!(!CommandHandler::is_default_command("!ping"));
        assert!(!CommandHandler::is_default_command("!switch foo"));
        assert!(!CommandHandler::is_default_command("default")); // no !
    }

    #[test]
    fn test_default_resets_to_config_default_after_switch() {
        let h = make_handler();
        // Switch away from default
        h.handle_switch("!switch custodian", "brian");
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));

        // !default should reset to librarian (brian's configured default)
        let reply = h.handle_default("brian");
        assert!(reply.contains("librarian"), "reply should name the default agent: {}", reply);
        assert!(reply.contains('✅'), "should be a success reply: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
    }

    #[test]
    fn test_default_is_idempotent_when_already_at_default() {
        let h = make_handler();
        // Already at librarian (the default) — !default should still succeed
        let reply = h.handle_default("brian");
        assert!(reply.contains('✅'), "!default from default should still succeed: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
    }

    #[test]
    fn test_default_no_routing_rule_returns_error() {
        let h = make_handler();
        let reply = h.handle_default("unknown_identity");
        assert!(reply.contains("⚠️"), "unknown identity should get error: {}", reply);
    }

    #[test]
    fn test_default_independent_per_identity() {
        let h = make_handler();
        h.handle_switch("!switch custodian", "brian");
        // Only reset brian; david should be unaffected
        h.handle_default("brian");
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    #[test]
    fn test_help_mentions_default_command() {
        let h = make_handler();
        let reply = h.handle("!help").unwrap();
        assert!(reply.contains("!default"), "help should mention !default: {}", reply);
    }
}
