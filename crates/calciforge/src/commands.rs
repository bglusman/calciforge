//! Local command handler for Calciforge.
//!
//! Commands starting with `!` are handled locally — they never reach the agent.
//! All other messages route to the agent as normal.
//!
//! # Command routing
//!
//! Some commands (`!help`, `!agents`, `!agent list`, `!metrics`, `!ping`)
//! require no
//! auth context and are intercepted before identity resolution.
//!
//! Other commands (`!switch`, `!agent switch`, `!status`) require an
//! authenticated identity and are handled after auth via
//! [`CommandHandler::handle_switch`] and
//! [`CommandHandler::cmd_status_for_identity`] respectively.

use std::collections::{HashMap, HashSet};
use std::fmt;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::sync::{Arc, AtomicU64, Mutex, Ordering};

use crate::adapters::{
    find_executable_for_agent, openclaw::SharedPendingApprovals, subprocess_command_for_agent,
};
use crate::config::CalciforgeConfig;
use crate::messages::{ChoiceControl, ChoiceOption, Match, OutboundMessage};
use crate::model_names::configured_first_class_model_ids;
use crate::providers::alloy::AlloyManager;

mod approvals;
mod model;
mod parser;
mod secure;
mod sessions;
mod state;

use parser::{command_suggestion, command_token, first_arg, second_arg};
#[cfg(test)]
use secure::{PasteServerEnv, paste_server_env_from_values, secure_input_target};
use secure::{secure_help, secure_input, secure_list, secure_set};
#[cfg(test)]
use sessions::active_sessions_message;
use state::{
    default_state_dir, load_active_agents_from, load_active_models_from, load_active_sessions_from,
    save_active_models_to,
};

const PENDING_CHOICE_TTL: Duration = Duration::from_secs(10 * 60);

fn acpx_binary_for_agent(agent_cfg: &crate::config::AgentConfig) -> Result<PathBuf, String> {
    find_executable_for_agent("acpx", agent_cfg.env.as_ref()).ok_or_else(|| {
        "acpx executable was not found on the effective PATH used for this agent; when env.PATH is configured it replaces Calciforge's service PATH".to_string()
    })
}

fn session_runtime_readiness_error(agent_cfg: &crate::config::AgentConfig) -> Option<String> {
    match agent_cfg.kind.as_str() {
        "acpx" => {
            if let Err(error) = acpx_binary_for_agent(agent_cfg) {
                return Some(error);
            }
            let Some(command) = subprocess_command_for_agent(agent_cfg) else {
                return Some("acpx agent is missing required command".to_string());
            };
            if find_executable_for_agent(command, agent_cfg.env.as_ref()).is_none() {
                return Some(format!(
                    "configured ACPX downstream command '{}' was not found on the effective PATH used for this agent; when env.PATH is configured it replaces Calciforge's service PATH",
                    command
                ));
            }
            None
        }
        "codex-cli" | "claude-cli" | "kimi-cli" => {
            let command = subprocess_command_for_agent(agent_cfg)
                .expect("cli session-capable agents have default commands");
            if find_executable_for_agent(command, agent_cfg.env.as_ref()).is_none() {
                return Some(format!(
                    "configured command '{}' was not found on the effective PATH used for this agent; when env.PATH is configured it replaces Calciforge's service PATH",
                    command
                ));
            }
            None
        }
        _ => None,
    }
}

fn gateway_model_selector_ids(config: &CalciforgeConfig) -> HashSet<String> {
    configured_first_class_model_ids(config)
        .into_iter()
        .map(|model| model.id)
        .chain(
            config
                .effective_model_shortcuts()
                .into_iter()
                .map(|shortcut| shortcut.alias),
        )
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentChoiceError {
    MissingRoutingRule {
        identity_id: String,
    },
    UnknownAllowedAgents {
        identity_id: String,
        unknown_agents: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingChoiceReply {
    Command(String),
    Reply(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtwRequest {
    pub agent_id: String,
    pub prompt: String,
}

impl fmt::Display for AgentChoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentChoiceError::MissingRoutingRule { identity_id } => {
                write!(f, "no routing rule found for identity '{identity_id}'")
            }
            AgentChoiceError::UnknownAllowedAgents {
                identity_id,
                unknown_agents,
            } => write!(
                f,
                "identity '{identity_id}' references unknown allowed_agents: {}",
                unknown_agents.join(", ")
            ),
        }
    }
}

/// In-memory command handler with simple counters and per-identity active-agent state.
pub struct CommandHandler {
    start_time: Instant,
    config: Arc<CalciforgeConfig>,
    messages_routed: AtomicU64,
    total_latency_ms: AtomicU64,
    /// Per-identity active agent: identity_id → agent_id.
    /// Persisted to `state_dir/active-agents.json` and loaded on startup.
    active_agents: Mutex<HashMap<String, String>>,
    /// Per-identity active gateway model selector: identity_id → model id.
    /// Persisted to `state_dir/active-models.json` and restored into the
    /// [`AlloyManager`] once it is attached.
    active_models: Mutex<HashMap<String, String>>,
    /// Per-identity, per-agent active downstream session selection.
    /// Persisted to `state_dir/active-agent-sessions.json`.
    active_sessions: Mutex<HashMap<String, HashMap<String, String>>>,
    /// Per-identity pending text fallback choice.
    ///
    /// This lets text-only channels accept "1", "2", or a label after a
    /// numbered choice list. It is intentionally short-lived and clears on any
    /// nonmatching reply so a later stray number cannot activate stale UI.
    pending_choices: Mutex<HashMap<String, PendingChoice>>,
    /// Directory for persisted state files.
    /// Defaults to `~/.config/calciforge/state/`; overridable for tests via
    /// [`CommandHandler::with_state_dir`].
    state_dir: PathBuf,
    /// Pending Clash approvals: request_id → ZeroClaw endpoint + metadata.
    /// Shared with any `ZeroClawHttpAdapter` instances created for the same agent
    /// so that `!approve` / `!deny` can signal the right ZeroClaw instance.
    pub pending_approvals: SharedPendingApprovals,
    /// reqwest client reused for approve/deny HTTP calls.
    http_client: reqwest::Client,
    /// Alloy manager for per-identity model/alloy selection.
    alloy_manager: Option<AlloyManager>,
    /// Local model lifecycle manager. When set, `!model <local-id>` triggers a
    /// local model switch (unload current, load new mlx_lm.server process).
    local_manager: Option<crate::sync::Arc<crate::local_model::LocalModelManager>>,
}

#[derive(Debug, Clone)]
struct PendingChoice {
    controls: Vec<ChoiceControl>,
    expires_at: Instant,
}

impl CommandHandler {
    /// Create a new CommandHandler, loading any persisted agent selections from disk.
    ///
    /// State is persisted to `~/.config/calciforge/state/`. For test isolation, use
    /// [`CommandHandler::with_state_dir`] to supply a per-test temp directory.
    pub fn new(config: Arc<CalciforgeConfig>) -> Self {
        Self::with_state_dir(config, default_state_dir())
    }

    /// Create a CommandHandler using a specific state directory.
    ///
    /// Allows tests to inject a temp directory so that persisted state
    /// (`active-agents.json`) does not bleed between test runs.
    pub fn with_state_dir(config: Arc<CalciforgeConfig>, state_dir: PathBuf) -> Self {
        let active_agents = load_active_agents_from(&state_dir);
        if !active_agents.is_empty() {
            tracing::info!(
                agents = ?active_agents,
                "loaded persisted active-agent selections"
            );
        }
        let active_models = load_active_models_from(&state_dir);
        if !active_models.is_empty() {
            tracing::info!(
                models = ?active_models,
                "loaded persisted active-model selections"
            );
        }
        let active_sessions = load_active_sessions_from(&state_dir);
        if !active_sessions.is_empty() {
            tracing::info!(
                sessions = ?active_sessions,
                "loaded persisted active session selections"
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
            active_models: Mutex::new(active_models),
            active_sessions: Mutex::new(active_sessions),
            pending_choices: Mutex::new(HashMap::new()),
            state_dir,
            pending_approvals: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            http_client,
            alloy_manager: None,
            local_manager: None,
        }
    }

    /// Set the gateway selector manager for this command handler.
    pub fn with_alloy_manager(mut self, manager: AlloyManager) -> Self {
        let mut configured_gateway_selectors = gateway_model_selector_ids(&self.config);
        configured_gateway_selectors.extend(
            manager
                .list_alloys()
                .into_iter()
                .map(|alloy| alloy.definition().id.clone()),
        );
        configured_gateway_selectors.extend(
            manager
                .list_cascades()
                .into_iter()
                .map(|cascade| cascade.id.clone()),
        );
        configured_gateway_selectors.extend(
            manager
                .list_dispatchers()
                .into_iter()
                .map(|dispatcher| dispatcher.id.clone()),
        );
        let mut active_models = self.active_models.lock().unwrap();
        active_models.retain(|identity_id, model_id| {
            if !configured_gateway_selectors.contains(model_id) {
                tracing::warn!(
                    identity = %identity_id,
                    model = %model_id,
                    "dropping persisted active-model selection that is not configured"
                );
                return false;
            }
            if manager.is_synthetic_model(model_id) {
                if let Err(err) = manager.set_active_for_identity(identity_id, model_id) {
                    tracing::warn!(
                        identity = %identity_id,
                        model = %model_id,
                        error = %err,
                        "failed to restore persisted active-model selection"
                    );
                    false
                } else {
                    true
                }
            } else {
                tracing::debug!(
                    identity = %identity_id,
                    model = %model_id,
                        "keeping persisted active-model selection outside gateway selector manager"
                );
                true
            }
        });
        save_active_models_to(&self.state_dir, &active_models);
        drop(active_models);
        self.alloy_manager = Some(manager);
        self
    }

    /// Set the local model manager for this command handler.
    pub fn with_local_manager(
        mut self,
        manager: crate::sync::Arc<crate::local_model::LocalModelManager>,
    ) -> Self {
        self.local_manager = Some(manager);
        self
    }

    /// Get a reference to the gateway selector manager, if configured.
    pub fn alloy_manager(&self) -> Option<&AlloyManager> {
        self.alloy_manager.as_ref()
    }

    /// Return agent choices the identity may activate, with display labels.
    pub fn agent_choices_for_identity(
        &self,
        identity_id: &str,
    ) -> Result<Vec<(String, String)>, AgentChoiceError> {
        let Some(routing_rule) = self
            .config
            .routing
            .iter()
            .find(|r| r.identity == identity_id)
        else {
            return Err(AgentChoiceError::MissingRoutingRule {
                identity_id: identity_id.to_string(),
            });
        };

        let allowed: Vec<&str> = if routing_rule.allowed_agents.is_empty() {
            self.config
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect()
        } else {
            routing_rule
                .allowed_agents
                .iter()
                .map(String::as_str)
                .collect()
        };

        let mut unknown_agents = Vec::new();
        let mut choices = Vec::new();
        for agent_id in allowed {
            match self.config.agents.iter().find(|agent| agent.id == agent_id) {
                Some(agent) => {
                    let label = agent
                        .registry
                        .as_ref()
                        .and_then(|registry| registry.display_name.as_deref())
                        .unwrap_or(&agent.id)
                        .to_string();
                    choices.push((agent.id.clone(), label));
                }
                None => unknown_agents.push(agent_id.to_string()),
            }
        }

        if unknown_agents.is_empty() {
            Ok(choices)
        } else {
            Err(AgentChoiceError::UnknownAllowedAgents {
                identity_id: identity_id.to_string(),
                unknown_agents,
            })
        }
    }

    /// Returns `true` for commands whose primary response can include agent choices.
    pub fn is_agent_choice_request(text: &str) -> bool {
        let mut tokens = text.split_whitespace();
        let Some(cmd) = tokens.next() else {
            return false;
        };
        let sub = tokens.next();
        if tokens.next().is_some() {
            return false;
        }

        match sub {
            None => cmd.eq_ignore_ascii_case("!agents") || cmd.eq_ignore_ascii_case("!agent"),
            Some(sub) => {
                cmd.eq_ignore_ascii_case("!agent")
                    && (sub.eq_ignore_ascii_case("list")
                        || sub.eq_ignore_ascii_case("ls")
                        || sub.eq_ignore_ascii_case("agents"))
            }
        }
    }

    /// Build a channel-agnostic agent choice response for an authenticated identity.
    pub fn agent_choice_message_for_identity(
        &self,
        text: &str,
        identity_id: &str,
    ) -> Option<OutboundMessage> {
        if !Self::is_agent_choice_request(text) {
            return None;
        }

        match self.agent_choices_for_identity(identity_id) {
            Ok(choices) => Some(
                OutboundMessage::text(
                    "Reply with a number, tap a button, or use `!agent details [agent]` for endpoints and adapter types.",
                )
                .with_control(ChoiceControl::new(
                    "Agents",
                    choices
                        .into_iter()
                        .map(|(id, label)| ChoiceOption::agent(label, id))
                        .collect(),
                )),
            ),
            Err(err) => {
                let base_reply = self
                    .handle(text)
                    .unwrap_or_else(|| "Configured agents unavailable.".to_string());
                Some(OutboundMessage::text(format!(
                    "{base_reply}\n\nButton choices unavailable: {err}."
                )))
            }
        }
    }

    /// Record that a message was routed to an agent.
    ///
    /// Call this after a successful agent dispatch with the measured latency.
    pub fn record_dispatch(&self, latency_ms: u64) {
        self.messages_routed.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);
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

    fn routing_rule_for_identity(&self, identity_id: &str) -> Option<&crate::config::RoutingRule> {
        self.config
            .routing
            .iter()
            .find(|rule| rule.identity == identity_id)
    }

    fn allowed_agent_ids_for_rule<'a>(
        &'a self,
        rule: &'a crate::config::RoutingRule,
    ) -> Vec<&'a str> {
        if rule.allowed_agents.is_empty() {
            self.config
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect()
        } else {
            rule.allowed_agents.iter().map(String::as_str).collect()
        }
    }

    fn resolve_allowed_agent_selector(&self, identity_id: &str, selector: &str) -> Option<String> {
        let rule = self.routing_rule_for_identity(identity_id)?;
        self.allowed_agent_ids_for_rule(rule)
            .into_iter()
            .find_map(|agent_id| {
                let agent = self
                    .config
                    .agents
                    .iter()
                    .find(|agent| agent.id == agent_id)?;
                if agent.id.eq_ignore_ascii_case(selector)
                    || agent
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(selector))
                {
                    Some(agent.id.clone())
                } else {
                    None
                }
            })
    }

    fn selector_matches_any_agent(&self, selector: &str) -> bool {
        self.config.agents.iter().any(|agent| {
            agent.id.eq_ignore_ascii_case(selector)
                || agent
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(selector))
        })
    }

    /// Parse `!btw <agent> <prompt>` or `!btw <prompt>` with a configured
    /// per-identity `btw_agent`. This does not mutate active agent/session
    /// state; channel handlers use it for one-off dispatch.
    pub fn parse_btw_command(&self, text: &str, identity_id: &str) -> Result<BtwRequest, String> {
        if !Self::is_btw_command(text) {
            return Err("Usage: !btw <agent> <prompt>".to_string());
        }

        let trimmed = text.trim();
        let rest = trimmed
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim())
            .unwrap_or("");
        if rest.is_empty() {
            return Err("Usage: !btw <agent> <prompt>\nOr configure routing.btw_agent and use !btw <prompt>.".to_string());
        }

        let Some(rule) = self.routing_rule_for_identity(identity_id) else {
            return Err(format!(
                "⚠️ No routing rule found for identity '{}'.",
                identity_id
            ));
        };

        let mut parts = rest.splitn(2, char::is_whitespace);
        let first = parts.next().unwrap_or("");
        let after_first = parts.next().map(str::trim).unwrap_or("");

        if let Some(agent_id) = self.resolve_allowed_agent_selector(identity_id, first) {
            if after_first.is_empty() {
                return Err(format!(
                    "Usage: !btw {} <prompt>\nThe one-off prompt cannot be empty.",
                    first
                ));
            }
            return Ok(BtwRequest {
                agent_id,
                prompt: after_first.to_string(),
            });
        }
        if self.selector_matches_any_agent(first) {
            return Err(format!("⚠️ Agent '{}' is not available to you.", first));
        }

        let Some(default_agent) = rule.btw_agent.as_deref() else {
            return Err("Usage: !btw <agent> <prompt>\nNo routing.btw_agent is configured for !btw <prompt>.".to_string());
        };
        let Some(agent_id) = self.resolve_allowed_agent_selector(identity_id, default_agent) else {
            return Err(format!(
                "⚠️ Configured btw_agent '{}' is not available to you.",
                default_agent
            ));
        };
        Ok(BtwRequest {
            agent_id,
            prompt: rest.to_string(),
        })
    }

    /// Remember the latest discrete choice sent to an identity.
    ///
    /// Channels call this immediately before/after sending an outbound message
    /// with controls. The next matching text reply can be converted back into
    /// the underlying command even on channels without native buttons.
    pub fn record_pending_choices(&self, identity_id: &str, message: &OutboundMessage) {
        if message.controls.is_empty() {
            return;
        }

        let pending = PendingChoice {
            controls: message.controls.clone(),
            expires_at: Instant::now() + PENDING_CHOICE_TTL,
        };
        self.pending_choices
            .lock()
            .unwrap()
            .insert(identity_id.to_string(), pending);
    }

    /// Resolve a text fallback choice reply into the command it represents.
    ///
    /// A successful match clears the pending choice and returns the command.
    /// A nonmatching reply also clears the pending choice and returns `None`,
    /// allowing normal message routing to continue. That fail-open-to-chat
    /// behavior avoids a stale pending list causing a future stray number to
    /// activate an old choice. Ambiguous label replies stay local and keep the
    /// choice active so the user can disambiguate with a number.
    pub fn resolve_pending_choice_reply(
        &self,
        identity_id: &str,
        text: &str,
    ) -> Option<PendingChoiceReply> {
        let mut pending_choices = self.pending_choices.lock().unwrap();
        let pending = pending_choices.get(identity_id)?;
        if Instant::now() >= pending.expires_at {
            pending_choices.remove(identity_id);
            return None;
        }

        let mut saw_numeric_attempt = false;
        let mut saw_ambiguous_attempt = false;
        for control in &pending.controls {
            match control.match_reply(text) {
                Match::One(option) => {
                    let command = option.command.clone();
                    pending_choices.remove(identity_id);
                    return Some(PendingChoiceReply::Command(command));
                }
                Match::OutOfRange => {
                    saw_numeric_attempt = true;
                }
                Match::Ambiguous => {
                    saw_ambiguous_attempt = true;
                }
                Match::None => {}
            }
        }

        if saw_ambiguous_attempt {
            return Some(PendingChoiceReply::Reply(
                "That matches more than one current choice. Reply with the number shown next to the option.".to_string(),
            ));
        }

        pending_choices.remove(identity_id);
        if saw_numeric_attempt {
            Some(PendingChoiceReply::Reply("⚠️ That number is not one of the current choices. Ask for the choices again if you still need them.".to_string()))
        } else {
            None
        }
    }

    /// Handle an identity-independent local command.
    ///
    /// Returns `Some(response)` if `text` starts with `!` and matches a known
    /// local command that is safe to answer after the channel has resolved a
    /// trusted sender identity. Returns `None` otherwise.
    ///
    /// **Note:** `!switch`, `!status`, and `!gateway` are intentionally NOT
    /// handled here. They need an explicit identity-resolved path so future
    /// pairing or room-based channels do not accidentally expose state before
    /// sender authorization.
    pub fn handle(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if !trimmed.starts_with('!') {
            return None;
        }

        // Grab just the command word (before any args)
        let cmd = command_token(trimmed).to_lowercase();

        match cmd.as_str() {
            "!help" => Some(self.cmd_help()),
            "!commands" => Some(self.cmd_help()),
            // !status needs auth — return None so the caller resolves identity first.
            "!status" => None,
            "!agents" => Some(self.cmd_agents_summary()),
            "!gateway" => None,
            "!metrics" => Some(self.cmd_metrics()),
            "!ping" => Some("pong".to_string()),
            // Session commands need auth — return None so caller resolves identity first.
            "!sessions" | "!session" | "!new" | "!btw" => None,
            // !switch needs auth — return None here so the caller can do auth
            // first, then call handle_switch().
            // !agent is an alias: reads as "pick an agent" since !agents lists them.
            "!switch" => None,
            "!agent" => {
                let sub = first_arg(trimmed).map(str::to_ascii_lowercase);
                match sub.as_deref() {
                    Some("list" | "ls" | "agents") | None => Some(self.cmd_agents_summary()),
                    Some("details" | "detail" | "info") => {
                        Some(self.cmd_agent_details(second_arg(trimmed)))
                    }
                    Some("help") => Some(self.cmd_help()),
                    _ => None,
                }
            }
            // !default needs auth — switches back to the configured default agent.
            "!default" => None,
            // !model shows model shortcuts/alloys — no auth needed for list.
            // Setting an alloy requires auth; handle_model() is called post-auth.
            "!model" => self.cmd_model_preauth(trimmed),
            // !secret is the noun-style alias for !secure and is handled
            // post-auth so the audit path and channel retention gate still run.
            "!secure" | "!secret" => None,
            _ => None, // Unknown !command — fall through to agent
        }
    }

    /// Returns `true` if the text is a `!sessions` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`handle_sessions`] instead of
    /// routing to the agent.
    pub fn is_sessions_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!sessions" || cmd == "!session"
    }

    /// Returns `true` if the text is a `!new` session command.
    pub fn is_new_session_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!new"
    }

    /// Returns `true` if the text is a `!btw` one-off dispatch command.
    pub fn is_btw_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!btw"
    }

    /// Returns `true` if the text is a `!switch` (or `!agent` alias) command.
    ///
    /// Use this AFTER auth to decide whether to call [`handle_switch`] instead of
    /// routing to the agent. `!agent <name>` reads naturally after `!agents` lists them.
    pub fn is_switch_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!switch" || cmd == "!agent"
    }

    /// Returns `true` if the text is a `!default` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`handle_default`] instead of
    /// routing to the agent.
    pub fn is_default_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!default"
    }

    /// Returns `true` if the text is a `!model` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`handle_model`] instead of
    /// routing to the agent.
    pub fn is_model_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!model"
    }

    /// Returns `true` if the text is a `!status` command (case-insensitive).
    ///
    /// Use this AFTER auth to decide whether to call [`cmd_status_for_identity`]
    /// instead of routing to the agent.
    pub fn is_status_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!status"
    }

    /// Returns `true` if the text is a `!gateway` command (case-insensitive).
    ///
    /// The gateway command can include operator-facing URLs, so channel
    /// handlers must only call [`cmd_gateway_for_identity`] after resolving a
    /// trusted sender identity.
    pub fn is_gateway_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!gateway"
    }

    /// Returns `true` if the text is an `!approve` command (case-insensitive).
    pub fn is_approve_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!approve"
    }

    /// Returns `true` if the text is a `!deny` command (case-insensitive).
    pub fn is_deny_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!deny"
    }

    /// Returns `true` if the text is the inline context-clear command.
    ///
    /// This command is handled by channel dispatchers because context storage is
    /// channel/thread scoped. Keep this predicate next to the other command
    /// classifiers so every channel agrees it is a known command and does not
    /// route it to the agent or reply with the generic unknown-command message.
    pub fn is_context_clear_command(text: &str) -> bool {
        text.trim().eq_ignore_ascii_case("!context clear")
    }

    /// Return true if the text starts with '!' (a command).
    pub fn is_command(text: &str) -> bool {
        text.trim().starts_with('!')
    }

    /// Returns `true` for simple local command tokens that [`handle`] answers
    /// directly.
    ///
    /// This is intentionally only the small token-level subset that needs no
    /// subcommand inspection. Other locally handled commands such as
    /// `!agent list` and `!model` are classified by their own command-specific
    /// predicates because related subcommands may need identity context.
    pub fn is_simple_local_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        matches!(
            cmd.as_str(),
            "!help" | "!commands" | "!agents" | "!metrics" | "!ping"
        )
    }

    /// Returns `true` if the text is a `!secret` / `!secure` command (case-insensitive).
    ///
    /// Secret commands are intercepted at the channel layer **before** any agent
    /// sees the message, so the raw value passed to
    /// `!secret set NAME=value` / `!secure set NAME=value` is never routed to an agent's context.
    /// It is still seen by the chat transport (Telegram/Matrix/WhatsApp
    /// logs), which is the retention-leak tradeoff the user has to
    /// accept (or use an out-of-band path). Document this in any UX
    /// that advertises secret commands.
    pub fn is_secure_command(text: &str) -> bool {
        let trimmed = text.trim();
        let cmd = command_token(trimmed).to_lowercase();
        cmd == "!secure" || cmd == "!secret"
    }

    /// Returns `true` only for the chat-value entry form
    /// `!secret set ...` / `!secure set ...`. This is the risky subcommand that channel
    /// handlers gate behind `allow_chat_secret_set`.
    pub fn is_secure_set_command(text: &str) -> bool {
        let mut parts = text.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let sub = parts.next().unwrap_or("").to_lowercase();
        (cmd == "!secure" || cmd == "!secret") && sub == "set"
    }

    /// Returns `true` for commands that are recognized after identity
    /// resolution by the shared channel layer or answered locally by
    /// [`handle`].
    ///
    /// [`handle`] covers identity-independent commands such as `!help`,
    /// `!agents`, `!metrics`, and `!ping`. Channel dispatchers use this helper
    /// for the remaining authenticated/inline commands before deciding whether
    /// an unhandled bang-prefixed message should receive the unknown-command
    /// reply.
    pub fn is_known_channel_command(text: &str) -> bool {
        Self::is_simple_local_command(text)
            || Self::is_status_command(text)
            || Self::is_gateway_command(text)
            || Self::is_switch_command(text)
            || Self::is_default_command(text)
            || Self::is_sessions_command(text)
            || Self::is_new_session_command(text)
            || Self::is_btw_command(text)
            || Self::is_model_command(text)
            || Self::is_secure_command(text)
            || Self::is_approve_command(text)
            || Self::is_deny_command(text)
            || Self::is_context_clear_command(text)
    }

    /// Returns `true` when a channel should send the generic unknown-command
    /// reply instead of routing the message to an agent.
    pub fn is_unknown_channel_command(text: &str) -> bool {
        Self::is_command(text) && !Self::is_known_channel_command(text)
    }

    /// Reply used when a channel has not opted into chat-transport
    /// secret values. Keep this value-free and channel-generic.
    pub fn secure_set_disabled_reply(channel_kind: &str) -> String {
        format!(
            "⚠️ `!secret set` / `!secure set` is disabled for {channel_kind} by default because \
             it sends the secret through chat history. Use `!secret input NAME` \
             from chat, or set `allow_chat_secret_set = true` on this channel \
             only if you accept that retention tradeoff."
        )
    }

    /// Respond to unknown commands with a helpful message.
    pub fn unknown_command(&self, text: &str) -> String {
        let cmd = command_token(text).to_string();
        let mut lines = vec![format!("⚠️ Unknown command: {cmd}")];
        if let Some(suggestion) = command_suggestion(&cmd) {
            lines.push(format!("Did you mean `{suggestion}`?"));
        }
        lines.push("Use `!help` to see available commands.".to_string());
        lines.join("\n\n")
    }

    /// Return a status string for the given authenticated identity.
    ///
    /// Uses [`active_agent_for`] to show the per-identity active agent rather
    /// than blindly reading the first routing rule's `default_agent`.
    ///
    /// When the active agent's adapter supports [`AgentAdapter::get_runtime_status`],
    /// this method queries the underlying agent for accurate runtime model/provider
    /// info (including alloy constituents) rather than relying on static config.
    pub async fn cmd_status_for_identity(&self, identity_id: &str) -> String {
        let uptime = self.start_time.elapsed();
        let uptime_secs = uptime.as_secs();
        let hours = uptime_secs / 3600;
        let minutes = (uptime_secs % 3600) / 60;
        let seconds = uptime_secs % 60;

        let version = self.config.calciforge.version;
        let agent_count = self.config.agents.len();
        let identity_count = self.config.identities.len();
        let channel_count = self.config.channels.len();

        // Use the real per-identity active agent (respects !switch overrides).
        let active_agent = self
            .active_agent_for(identity_id)
            .unwrap_or_else(|| "none".to_string());
        let active_model = self.active_model_for_identity(identity_id);
        let active_model_info = active_model
            .as_deref()
            .map(|model| format!("\n  active model override: {model}"))
            .unwrap_or_else(|| "\n  active model override: none".to_string());

        // Try to get runtime status from the adapter (for ZeroClaw and others that support it)
        let runtime_info =
            if let Some(agent_cfg) = self.config.agents.iter().find(|a| a.id == active_agent) {
                match crate::adapters::build_adapter(agent_cfg) {
                    Ok(adapter) => {
                        if let Some(status) = adapter.get_runtime_status().await {
                            // Format runtime status with alloy constituents if present
                            let constituents_str = status
                                .alloy_constituents
                                .as_ref()
                                .map(|constituents| {
                                    let parts: Vec<String> = constituents
                                        .iter()
                                        .map(|(prov, model)| format!("    - {prov}: {model}"))
                                        .collect();
                                    format!("\n  constituents:\n{}", parts.join("\n"))
                                })
                                .unwrap_or_default();

                            format!(
                                "\n  provider: {}\n  model: {}{}",
                                status.provider, status.model, constituents_str
                            )
                        } else {
                            // Adapter doesn't support runtime status, fall back to config
                            let model = agent_cfg.model.as_deref().unwrap_or("default");
                            let provider = &agent_cfg.kind;
                            if provider.contains("alloy") || model.contains("alloy") {
                                format!("\n  provider: {provider} (alloy)\n  model: {model}")
                            } else {
                                format!("\n  provider: {provider}\n  model: {model}")
                            }
                        }
                    }
                    Err(_) => {
                        // Failed to build adapter, use config
                        let model = agent_cfg.model.as_deref().unwrap_or("default");
                        let provider = &agent_cfg.kind;
                        format!("\n  provider: {provider}\n  model: {model}")
                    }
                }
            } else {
                String::new()
            };

        // Build per-agent model summary: "librarian (claude-sonnet-4-6), max (default)"
        let agent_summary: Vec<String> = self
            .config
            .agents
            .iter()
            .map(|a| {
                let model = a.model.as_deref().unwrap_or("default");
                format!("{} ({})", a.id, model)
            })
            .collect();
        let agents_display = if agent_summary.is_empty() {
            format!("{agent_count} agents")
        } else {
            agent_summary.join(", ")
        };

        format!(
            "Calciforge status:\n  version: {version}\n  uptime: {hours}h {minutes}m {seconds}s\n  active agent: {active_agent}{active_model_info}{runtime_info}\n  agents: {agents_display}\n  identities: {identity_count}, channels: {channel_count}"
        )
    }

    /// Handle `!secret <subcommand>` / `!secure <subcommand>`. Secret values never transit an
    /// agent's context. `set` is the legacy chat-retained path;
    /// `input`/`bulk` mint short-lived paste URLs.
    ///
    /// Subcommands:
    ///   - `!secure set NAME=value`  — store a secret via chat (legacy/caution)
    ///   - `!secure input NAME` / `!secret input NAME` — create a paste URL
    ///   - `!secure bulk [description]` / `!secret bulk [description]` — create a `.env` paste URL
    ///   - `!secure list` / `!secret list`             — list stored secret names
    ///   - `!secure help` / `!secret help`             — usage string
    ///
    /// **Retention warning** (documented in first-time-use UX):
    /// this command's text passes through the chat transport
    /// (Telegram/Matrix/WhatsApp), which retains message history.
    /// For values where chat-transport exposure is unacceptable, use
    /// `!secure input NAME` or run `paste-server NAME` locally.
    pub async fn handle_secure(&self, text: &str, identity_id: &str) -> String {
        let trimmed = text.trim();
        // `!secret ...` / `!secure ...` — split off the subcommand word using
        // split_whitespace so multiple spaces / tabs don't end up as
        // empty middle tokens (the prior splitn(' ') treated
        // "!secure  set NAME=v" as sub="" with the rest mis-shaped).
        let mut parts = trimmed.split_whitespace();
        let lead = parts.next().unwrap_or("!secure").to_lowercase();
        let sub = parts.next().map(|s| s.to_lowercase()).unwrap_or_default();
        // Reconstruct the rest by joining remaining tokens with single
        // spaces. For chat `set`, the value goes through to fnox set
        // (now via stdin) so internal whitespace shape is preserved by
        // the caller that builds it as `NAME=value`.
        let rest_owned: String = parts.collect::<Vec<_>>().join(" ");
        let rest = rest_owned.trim();

        // Audit-log who invoked which subcommand. NEVER log `rest` —
        // that contains the secret value for `set`. Identity is the
        // chat-side principal; correlatable to channel + auth.
        tracing::info!(
            identity = %identity_id,
            command = %lead,
            subcommand = %sub,
            "secure command invoked"
        );

        match sub.as_str() {
            "set" => secure_set(rest).await,
            "input" | "request" => secure_input(rest, false).await,
            "bulk" => secure_input(rest, true).await,
            "list" => secure_list().await,
            "help" | "" => secure_help(),
            _ => format!("⚠️ Unknown {lead} subcommand: `{sub}`\n\n{}", secure_help()),
        }
    }

    // -----------------------------------------------------------------------
    // Individual command handlers
    // -----------------------------------------------------------------------

    pub fn cmd_gateway_for_identity(&self, _identity_id: &str) -> String {
        let Some(proxy) = self.config.proxy.as_ref() else {
            return "Model gateway: disabled.".to_string();
        };

        let mut lines = vec![
            "Model gateway:".to_string(),
            format!("  enabled: {}", proxy.enabled),
            format!("  engine: {}", proxy.backend_type),
            format!("  bind: {}", proxy.bind),
        ];

        if let Some(url) = proxy
            .gateway_ui_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            lines.push(format!("  UI: {url}"));
            lines.push("  Calciforge redirect: /gateway/ui on the model gateway host".to_string());
        } else {
            lines.push("  UI: not configured".to_string());
        }

        lines.join("\n")
    }

    fn cmd_help(&self) -> String {
        let lines = vec![
            "Calciforge — available commands:",
            "  !help, !commands — show this help",
            "  !status  — version, uptime, active agent, config summary",
            "  !agents  — list configured agents",
            "  !sessions <agent> — list downstream sessions when the adapter supports it",
            "  !new [session] — start a new named session with your active agent",
            "  !btw <agent> <prompt> — ask another agent once without switching",
            "  !gateway — model gateway engine, bind address, and UI link",
            "  !metrics — messages routed, average latency",
            "  !ping    — connectivity check (replies: pong)",
            "  !switch, !agent <agent> [session] — switch active agent (requires auth)",
            "  !agent list | !agent details [agent] | !agent switch <agent> — noun-style agent commands",
            "  !default — switch back to your default agent (requires auth)",
            "  !model [list|use <id>|alias] — show or activate model choices",
            "  !secure, !secret <input|bulk|list|help> — paste URLs and secret names; `set` is legacy fallback",
            "  !approve [request_id] — approve a pending Clash tool call",
            "  !deny [request_id] [reason] — deny a pending Clash tool call",
        ];
        lines.join("\n")
    }
    fn cmd_agents_summary(&self) -> String {
        if self.config.agents.is_empty() {
            return "No agents configured.".to_string();
        }

        let mut lines = vec!["Agents:".to_string()];
        for agent in &self.config.agents {
            let model_info = agent.model.as_deref().unwrap_or("default");
            lines.push(format!("  {} — {}", agent.id, model_info));
        }
        lines.push("Use !agent details [agent] for endpoints and adapter types.".to_string());
        lines.join("\n")
    }

    fn cmd_agent_details(&self, agent_id: Option<&str>) -> String {
        if self.config.agents.is_empty() {
            return "No agents configured.".to_string();
        }

        let agents: Vec<_> = self
            .config
            .agents
            .iter()
            .filter(|agent| agent_id.is_none_or(|id| agent.id == id))
            .collect();
        if agents.is_empty() {
            let requested = agent_id.unwrap_or_default();
            return format!(
                "⚠️ Agent '{requested}' not found. Use !agent list to see available agents."
            );
        }

        let heading = if let Some(agent_id) = agent_id {
            format!("Agent details: {agent_id}")
        } else {
            "Agent details:".to_string()
        };
        let mut lines = vec![heading];
        for agent in agents {
            let location = if agent.kind == "cli" {
                agent.command.as_deref().unwrap_or("(no command)")
            } else if agent.endpoint.is_empty() {
                "(no endpoint)"
            } else {
                &agent.endpoint
            };
            let model_info = agent.model.as_deref().unwrap_or("default");
            lines.push(format!(
                "  {} ({}, model: {}) — {}",
                agent.id, agent.kind, model_info, location
            ));
        }
        lines.join("\n")
    }

    fn cmd_metrics(&self) -> String {
        let routed = self.messages_routed.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ms.load(Ordering::Relaxed);
        let avg_latency = total_latency.checked_div(routed).unwrap_or(0);

        format!("Calciforge metrics:\n  messages routed: {routed}\n  avg latency: {avg_latency}ms")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // SECURE_ENV_MUTEX is held across awaits in the !secure tests.
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, AgentRegistry, AlloyConfig, AlloyConstituentConfig, CalciforgeConfig,
        CalciforgeHeader, CascadeConfig, ChannelAlias, ChannelConfig, DispatcherConfig, Identity,
        ModelRoleConfig, ModelShortcutConfig, RoutingRule, SyntheticModelConfig,
    };
    use crate::providers::alloy::AlloyManager;
    use mockito::Matcher;

    fn make_handler() -> CommandHandler {
        let config = Arc::new(make_config());
        make_handler_with_config(config)
    }

    fn make_handler_with_config(config: Arc<CalciforgeConfig>) -> CommandHandler {
        // Use a per-test temp directory so persisted state (`active-agents.json`)
        // never bleeds between test runs.  Without this, a test that calls
        // `handle_switch` writes to the shared active-agent state file
        // file, causing subsequent tests that construct a fresh handler to observe
        // the leftover switch state.
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
    }

    fn test_executable_name(name: &str) -> String {
        #[cfg(windows)]
        {
            format!("{name}.cmd")
        }
        #[cfg(not(windows))]
        {
            name.to_string()
        }
    }

    fn write_test_executable(dir: &Path, name: &str) {
        let path = dir.join(test_executable_name(name));
        #[cfg(windows)]
        let contents = "@echo off\r\nexit /b 0\r\n";
        #[cfg(not(windows))]
        let contents = "#!/bin/sh\nexit 0\n";
        std::fs::write(&path, contents).expect("write test executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }

    fn pending_approval(
        request_id: &str,
        endpoint: String,
    ) -> crate::adapters::openclaw::PendingApprovalMeta {
        crate::adapters::openclaw::PendingApprovalMeta {
            request_id: request_id.to_string(),
            zeroclaw_endpoint: endpoint,
            zeroclaw_auth_token: "approval-token".to_string(),
            _summary: "test approval".to_string(),
        }
    }

    fn set_agent_path(config: &mut CalciforgeConfig, agent_id: &str, path: &Path) {
        let agent = config
            .agents
            .iter_mut()
            .find(|agent| agent.id == agent_id)
            .unwrap_or_else(|| panic!("test config has agent {agent_id}"));
        agent.env = Some(HashMap::from([(
            "PATH".to_string(),
            path.display().to_string(),
        )]));
    }

    fn force_active_agent(handler: &CommandHandler, identity_id: &str, agent_id: &str) {
        handler
            .active_agents
            .lock()
            .unwrap()
            .insert(identity_id.to_string(), agent_id.to_string());
    }

    fn synthetic_manager() -> AlloyManager {
        AlloyManager::from_gateway_configs(
            &[AlloyConfig {
                id: "alloy-test".to_string(),
                name: "Test Alloy".to_string(),
                strategy: "round_robin".to_string(),
                constituents: vec![
                    AlloyConstituentConfig {
                        model: "gpt-4".to_string(),
                        weight: 1,
                        context_window: 128_000,
                    },
                    AlloyConstituentConfig {
                        model: "claude-3-5-sonnet".to_string(),
                        weight: 1,
                        context_window: 128_000,
                    },
                ],
                min_context_window: None,
            }],
            &[CascadeConfig {
                id: "cascade-test".to_string(),
                name: Some("Test Cascade".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "gpt-4".to_string(),
                    context_window: 128_000,
                }],
            }],
            &[DispatcherConfig {
                id: "dispatcher-test".to_string(),
                name: Some("Test Dispatcher".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "gpt-4".to_string(),
                    context_window: 128_000,
                }],
            }],
        )
        .expect("synthetic manager")
    }

    fn make_handler_with_synthetics() -> CommandHandler {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let handler = CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager());
        let switched = handler.handle_switch("!switch gateway", "brian");
        assert!(
            switched.contains("gateway"),
            "model tests require an override-capable active agent: {switched}"
        );
        handler
    }

    fn make_config() -> CalciforgeConfig {
        CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![
                Identity {
                    id: "brian".to_string(),
                    display_name: Some("Brian".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "7000000001".to_string(),
                    }],
                    role: Some("owner".to_string()),
                },
                Identity {
                    id: "david".to_string(),
                    display_name: Some("David".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "7000000002".to_string(),
                    }],
                    role: Some("user".to_string()),
                },
            ],
            agents: vec![
                AgentConfig {
                    id: "librarian".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://example.invalid:18789".to_string(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
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
                    registry: Some(AgentRegistry {
                        display_name: Some("Librarian".to_string()),
                        ..Default::default()
                    }),
                    aliases: vec![],
                },
                AgentConfig {
                    id: "custodian".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://10.0.0.50:18789".to_string(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
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
                    aliases: vec!["keeper".to_string(), "cust".to_string()],
                },
                AgentConfig {
                    id: "claude-acpx".to_string(),
                    kind: "acpx".to_string(),
                    endpoint: String::new(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: None,
                    api_key: None,
                    api_key_file: None,
                    openclaw_agent_id: None,
                    allow_model_override: None,
                    reply_port: None,
                    reply_auth_token: None,
                    reply_auth_token_file: None,
                    command: Some("claude".to_string()),
                    args: None,
                    env: None,
                    registry: None,
                    aliases: vec!["claude".to_string()],
                },
                AgentConfig {
                    id: "gateway".to_string(),
                    kind: "openai-compat".to_string(),
                    endpoint: "http://127.0.0.1:18083/v1".to_string(),
                    timeout_ms: Some(120000),
                    model: None,
                    auth_token: None,
                    api_key: Some("test-gateway-key".to_string()),
                    api_key_file: None,
                    openclaw_agent_id: None,
                    allow_model_override: Some(true),
                    reply_port: None,
                    reply_auth_token: None,
                    reply_auth_token_file: None,
                    command: None,
                    args: None,
                    env: None,
                    registry: Some(AgentRegistry {
                        display_name: Some("Gateway".to_string()),
                        ..Default::default()
                    }),
                    aliases: vec![],
                },
                AgentConfig {
                    id: "codex".to_string(),
                    kind: "codex-cli".to_string(),
                    endpoint: String::new(),
                    timeout_ms: Some(120000),
                    model: Some("gpt-5.5".to_string()),
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
                    registry: Some(AgentRegistry {
                        display_name: Some("Codex".to_string()),
                        ..Default::default()
                    }),
                    aliases: vec!["code".to_string()],
                },
            ],
            routing: vec![
                RoutingRule {
                    identity: "brian".to_string(),
                    default_agent: "librarian".to_string(),
                    btw_agent: None,
                    allowed_agents: vec![], // unrestricted
                },
                RoutingRule {
                    identity: "david".to_string(),
                    default_agent: "librarian".to_string(),
                    btw_agent: None,
                    allowed_agents: vec!["librarian".to_string()], // restricted
                },
            ],
            channels: vec![ChannelConfig {
                kind: "telegram".to_string(),
                bot_token_file: Some("~/.config/calciforge/secrets/telegram-token".to_string()),
                enabled: true,
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            model_roles: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            security: None,
            proxy: Some(crate::config::ProxyConfig {
                enabled: true,
                bind: "127.0.0.1:18083".to_string(),
                backend_type: "http".to_string(),
                gateway_ui_url: Some("http://127.0.0.1:8585".to_string()),
                ..Default::default()
            }),
            local_models: None,
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

    #[test]
    fn unknown_command_suggests_near_matches() {
        let h = make_handler();
        let reply = h.unknown_command("!stats");
        assert!(reply.contains("Unknown command"), "{reply}");
        assert!(reply.contains("!status"), "{reply}");
        assert!(reply.contains("!help"), "{reply}");
    }

    #[test]
    fn unknown_command_skips_suggestion_for_oversized_token() {
        let h = make_handler();
        let oversized = format!("!{}", "x".repeat(256));
        let reply = h.unknown_command(&oversized);
        assert!(reply.contains("Unknown command"), "{reply}");
        assert!(
            !reply.contains("Did you mean"),
            "oversized unknown commands should not run fuzzy matching: {reply}"
        );
    }

    #[test]
    fn unknown_command_suggestion_handles_tab_after_command() {
        let h = make_handler();
        let reply = h.unknown_command("!stats\tplease");
        assert!(reply.contains("Unknown command"), "{reply}");
        assert!(reply.contains("!status"), "{reply}");
    }

    // --- !help ---

    #[test]
    fn test_help_contains_all_commands() {
        let h = make_handler();
        let reply = h.handle("!help").unwrap();
        assert!(reply.contains("!help"));
        assert!(reply.contains("!status"));
        assert!(reply.contains("!agents"));
        assert!(reply.contains("!gateway"));
        assert!(reply.contains("!metrics"));
        assert!(reply.contains("!ping"));
        assert!(reply.contains("!switch"));
        assert!(reply.contains("!agent list"));
        assert!(reply.contains("!secret"));
        assert!(!reply.contains("Gateway UI:"));
        assert!(!reply.contains("http://127.0.0.1:8585"));
    }

    #[test]
    fn gateway_command_reports_engine_and_ui_link() {
        let h = make_handler();
        let reply = h.cmd_gateway_for_identity("brian");

        assert!(reply.contains("Model gateway:"), "{reply}");
        assert!(reply.contains("engine: http"), "{reply}");
        assert!(reply.contains("bind: 127.0.0.1:18083"), "{reply}");
        assert!(reply.contains("UI: http://127.0.0.1:8585"), "{reply}");
        assert!(reply.contains("/gateway/ui"), "{reply}");
    }

    #[test]
    fn gateway_command_reports_helicone_engine_and_dashboard_link() {
        let mut config = make_config();
        let proxy = config.proxy.as_mut().expect("test proxy config");
        proxy.backend_type = "helicone".to_string();
        proxy.backend_url = "https://ai-gateway.helicone.ai".to_string();
        proxy.gateway_ui_url = Some("https://us.helicone.ai/requests".to_string());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let h = CommandHandler::with_state_dir(Arc::new(config), tmp.path().to_path_buf());

        let reply = h.cmd_gateway_for_identity("brian");

        assert!(reply.contains("engine: helicone"), "{reply}");
        assert!(
            reply.contains("UI: https://us.helicone.ai/requests"),
            "{reply}"
        );
        assert!(reply.contains("/gateway/ui"), "{reply}");
    }

    #[test]
    fn gateway_command_is_not_identity_independent() {
        let h = make_handler();
        assert!(
            h.handle("!gateway").is_none(),
            "!gateway must be handled only after sender identity resolution"
        );
        assert!(CommandHandler::is_gateway_command("!GATEWAY"));
    }

    // --- !status ---

    #[test]
    fn test_status_handle_returns_none_without_identity_context() {
        // !status must NOT be handled by the identity-independent helper.
        let h = make_handler();
        assert!(
            h.handle("!status").is_none(),
            "!status must return None from handle()"
        );
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
    fn channel_command_classification_keeps_known_commands_out_of_unknown_path() {
        for command in [
            "!help",
            "!commands",
            "!agents",
            "!agent list",
            "!agent custodian",
            "!metrics",
            "!ping",
            "!status",
            "!gateway",
            "!switch custodian",
            "!default",
            "!sessions codex",
            "!session list codex",
            "!new",
            "!btw codex say hi",
            "!model",
            "!secure input API_KEY",
            "!secret list",
            "!approve 1",
            "!deny 1",
            "!context clear",
        ] {
            assert!(
                CommandHandler::is_known_channel_command(command),
                "{command} should be recognized by the shared channel classifier"
            );
            assert!(
                !CommandHandler::is_unknown_channel_command(command),
                "{command} should not be handled as an unknown command"
            );
        }
    }

    #[test]
    fn channel_command_classification_still_flags_unknown_bang_commands() {
        assert!(CommandHandler::is_unknown_channel_command("!wat"));
        assert!(CommandHandler::is_unknown_channel_command("  !statuz  "));
        assert!(!CommandHandler::is_unknown_channel_command("hello agent"));
    }

    #[test]
    fn session_alias_detection_accepts_singular_form() {
        assert!(CommandHandler::is_sessions_command("!sessions claude-acpx"));
        assert!(CommandHandler::is_sessions_command(
            "!session list claude-acpx"
        ));
        assert!(CommandHandler::is_sessions_command(
            "  !SESSION list claude-acpx"
        ));
        assert!(!CommandHandler::is_sessions_command(
            "session list claude-acpx"
        ));
    }

    #[tokio::test]
    async fn session_list_alias_parses_agent_after_list_verb() {
        let h = make_handler();
        let reply = h.handle_sessions("!session list codex", "brian").await;
        assert!(
            reply.contains("codex") && reply.contains("supports named sessions"),
            "noun-style session alias should parse the agent after 'list': {reply}"
        );
    }

    #[test]
    fn shared_choice_messages_cover_agent_model_session_and_approval_actions() {
        let h = make_handler_with_synthetics();

        let agents = h
            .agent_choice_message_for_identity("!agent list", "brian")
            .expect("agent choices");
        assert!(
            agents
                .controls
                .iter()
                .flat_map(|control| &control.options)
                .any(|option| option.command == "!agent switch librarian"
                    && option.callback_data.as_deref() == Some("cf:agent:librarian")),
            "agent choices must provide matching text and callback actions: {agents:?}"
        );
        let agent_fallback = agents.render_text_fallback();
        assert!(
            agent_fallback.contains("\n1. Librarian: `!agent switch librarian`"),
            "agent choice fallback must present numbered commands near the visible choice list: {agent_fallback}"
        );
        assert!(
            !agent_fallback.contains("\n  librarian —"),
            "agent choice fallback should not start with the older unnumbered summary list: {agent_fallback}"
        );
        assert!(
            agent_fallback.contains("!agent details [agent]"),
            "agent choice fallback should keep detail-command guidance: {agent_fallback}"
        );

        let models = h.model_choice_message("!model").expect("model choices");
        assert!(
            models
                .controls
                .iter()
                .flat_map(|control| &control.options)
                .any(|option| option.command == "!model use dispatcher-test"
                    && option.callback_data.as_deref() == Some("cf:model:dispatcher-test")),
            "model choices must provide matching text and callback actions: {models:?}"
        );

        let sessions = active_sessions_message(
            "claude-acpx",
            vec![
                "backend".to_string(),
                "../bad".to_string(),
                "review".to_string(),
            ],
        );
        assert!(
            sessions
                .controls
                .iter()
                .flat_map(|control| &control.options)
                .any(|option| option.command == "!switch claude-acpx backend"
                    && option.callback_data.as_deref() == Some("cf:session:claude-acpx:backend")),
            "session choices must provide matching text and callback actions: {sessions:?}"
        );
        let session_fallback = sessions.render_text_fallback();
        assert!(
            !session_fallback.contains("../bad"),
            "session choices must not present names rejected by !switch validation: {session_fallback}"
        );

        let approval = CommandHandler::approval_request_message("rm -rf /tmp/x", "test", "req-1");
        let fallback = approval.render_text_fallback();
        assert!(
            fallback.contains("!approve req-1") && fallback.contains("!deny req-1"),
            "approval fallback must remain actionable on text-only channels: {fallback}"
        );
        assert!(
            approval
                .controls
                .iter()
                .flat_map(|control| &control.options)
                .any(|option| option.command == "!approve req-1"
                    && option.callback_data.as_deref() == Some("cf:approve:req-1")),
            "approval choice must expose approve callback: {approval:?}"
        );
        assert!(
            approval
                .controls
                .iter()
                .flat_map(|control| &control.options)
                .any(|option| option.command == "!deny req-1"
                    && option.callback_data.as_deref() == Some("cf:deny:req-1")),
            "approval choice must expose deny callback: {approval:?}"
        );
    }

    #[tokio::test]
    async fn approve_parses_request_id_with_repeated_whitespace() {
        let mut server = mockito::Server::new_async().await;
        let approval = server
            .mock("POST", "/webhook/approve")
            .match_header("authorization", "Bearer approval-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "request_id": "req-1",
                "approved": true
            })))
            .with_status(503)
            .with_body("stop before polling")
            .create_async()
            .await;

        let h = make_handler();
        h.register_pending_approval(pending_approval("req-1", server.url()))
            .await;
        h.register_pending_approval(pending_approval("req-2", "http://127.0.0.1:9".to_string()))
            .await;

        let (reply, follow_up) = h.handle_approve("!approve   req-1").await;

        approval.assert_async().await;
        assert!(follow_up.is_none());
        assert!(
            reply.contains("Failed to send approval"),
            "explicit request id should be honored despite repeated spaces: {reply}"
        );
        assert!(
            !reply.contains("pending approvals"),
            "repeated spaces must not drop the explicit request id: {reply}"
        );
    }

    #[tokio::test]
    async fn deny_accepts_non_uuid_request_id_with_reason() {
        let mut server = mockito::Server::new_async().await;
        let approval = server
            .mock("POST", "/webhook/approve")
            .match_header("authorization", "Bearer approval-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "request_id": "req-1",
                "approved": false,
                "reason": "not today"
            })))
            .with_status(503)
            .with_body("stop before polling")
            .create_async()
            .await;

        let h = make_handler();
        h.register_pending_approval(pending_approval("req-1", server.url()))
            .await;

        let (reply, follow_up) = h.handle_deny("!deny req-1 not today").await;

        approval.assert_async().await;
        assert!(follow_up.is_none());
        assert!(reply.contains("Failed to send denial"), "{reply}");
    }

    #[test]
    fn pending_choice_numeric_reply_resolves_and_clears() {
        let h = make_handler();
        let choices = h
            .agent_choice_message_for_identity("!agents", "brian")
            .expect("agent choice message");
        h.record_pending_choices("brian", &choices);

        assert_eq!(
            h.resolve_pending_choice_reply("brian", "2"),
            Some(PendingChoiceReply::Command(
                "!agent switch custodian".to_string()
            ))
        );
        assert_eq!(h.resolve_pending_choice_reply("brian", "1"), None);
    }

    #[test]
    fn pending_choice_nonmatching_reply_clears_and_falls_through() {
        let h = make_handler();
        let choices = h
            .agent_choice_message_for_identity("!agents", "brian")
            .expect("agent choice message");
        h.record_pending_choices("brian", &choices);

        assert_eq!(h.resolve_pending_choice_reply("brian", "hello later"), None);
        assert_eq!(h.resolve_pending_choice_reply("brian", "1"), None);
    }

    #[test]
    fn pending_choice_out_of_range_number_clears_with_reply() {
        let h = make_handler();
        let choices = h
            .agent_choice_message_for_identity("!agents", "brian")
            .expect("agent choice message");
        h.record_pending_choices("brian", &choices);

        let reply = h.resolve_pending_choice_reply("brian", "99");
        assert!(
            matches!(reply, Some(PendingChoiceReply::Reply(ref message)) if message.contains("not one of the current choices")),
            "out-of-range numeric choice should produce a bounded local reply, got: {reply:?}"
        );
        assert_eq!(h.resolve_pending_choice_reply("brian", "1"), None);
    }

    #[test]
    fn pending_choice_ambiguous_label_reprompts_and_keeps_choice() {
        let h = make_handler();
        let choices = OutboundMessage::default().with_control(ChoiceControl::new(
            "Pick one",
            vec![
                ChoiceOption::new("Critic", "!agent switch critic"),
                ChoiceOption::new("Critique", "!agent switch critique"),
            ],
        ));
        h.record_pending_choices("brian", &choices);

        let reply = h.resolve_pending_choice_reply("brian", "cri");
        assert!(
            matches!(reply, Some(PendingChoiceReply::Reply(ref message)) if message.contains("more than one")),
            "ambiguous label should stay local and ask for a number, got: {reply:?}"
        );
        assert_eq!(
            h.resolve_pending_choice_reply("brian", "2"),
            Some(PendingChoiceReply::Command(
                "!agent switch critique".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn test_status_contains_version() {
        let h = make_handler();
        let reply = h.cmd_status_for_identity("brian").await;
        assert!(reply.contains("version: 2"), "should show version 2");
    }

    #[tokio::test]
    async fn test_status_contains_active_agent() {
        let h = make_handler();
        // Default (no switch): should show librarian
        let reply = h.cmd_status_for_identity("brian").await;
        assert!(
            reply.contains("librarian"),
            "should show active agent 'librarian'"
        );
    }

    #[tokio::test]
    async fn test_status_reflects_switch() {
        let h = make_handler();
        // Switch brian to custodian
        h.handle_switch("!switch custodian", "brian");
        let reply = h.cmd_status_for_identity("brian").await;
        assert!(
            reply.contains("custodian"),
            "status should reflect !switch: {}",
            reply
        );
        assert!(
            !reply.contains("librarian") || reply.contains("custodian"),
            "status should show switched agent: {}",
            reply
        );
    }

    #[tokio::test]
    async fn test_status_independent_per_identity() {
        let h = make_handler();
        h.handle_switch("!switch custodian", "brian");
        // brian switched to custodian — david should still see librarian
        let brian_reply = h.cmd_status_for_identity("brian").await;
        let david_reply = h.cmd_status_for_identity("david").await;
        assert!(
            brian_reply.contains("custodian"),
            "brian should see custodian: {}",
            brian_reply
        );
        assert!(
            david_reply.contains("librarian"),
            "david should still see librarian: {}",
            david_reply
        );
    }

    #[tokio::test]
    async fn test_status_contains_uptime() {
        let h = make_handler();
        let reply = h.cmd_status_for_identity("brian").await;
        assert!(reply.contains("uptime:"), "should contain uptime");
    }

    // --- !agents ---

    #[test]
    fn test_agents_lists_configured_agents() {
        let h = make_handler();
        let reply = h.handle("!agents").unwrap();
        assert!(reply.contains("librarian"), "should show agent id");
        assert!(
            !reply.contains("example.invalid"),
            "summary should not show noisy endpoint details: {reply}"
        );
        assert!(
            !reply.contains("openclaw-channel"),
            "summary should not show noisy adapter details: {reply}"
        );
        // Should show model info (fallback to "default" when no model set)
        assert!(reply.contains("default"), "should show model summary");
        assert!(
            reply.contains("!agent details"),
            "should point to detail command: {reply}"
        );
    }

    #[test]
    fn agent_list_alias_lists_configured_agents() {
        let h = make_handler();
        let reply = h.handle("!agent list").unwrap();
        assert!(reply.contains("librarian"), "should show agent id: {reply}");
        assert!(reply.contains("custodian"), "should show agent id: {reply}");

        let uppercase = h.handle("!AGENT LIST").unwrap();
        assert!(
            uppercase.contains("librarian"),
            "uppercase alias should also list agents: {uppercase}"
        );
    }

    #[test]
    fn agent_details_shows_endpoint_and_kind_metadata() {
        let h = make_handler();
        let reply = h.handle("!agent details librarian").unwrap();
        assert!(reply.contains("librarian"), "should show agent id: {reply}");
        assert!(
            reply.contains("example.invalid"),
            "details should show endpoint: {reply}"
        );
        assert!(
            reply.contains("openclaw-channel"),
            "details should show agent kind: {reply}"
        );
        assert!(
            !reply.contains("custodian"),
            "targeted details should only show requested agent: {reply}"
        );
    }

    #[test]
    fn agent_choices_report_missing_routing_rule() {
        let h = make_handler();
        let err = h
            .agent_choices_for_identity("unknown_identity")
            .unwrap_err();
        assert!(matches!(
            err,
            AgentChoiceError::MissingRoutingRule { identity_id }
                if identity_id == "unknown_identity"
        ));
    }

    #[test]
    fn agent_choices_report_unknown_allowed_agents() {
        let mut config = make_config();
        config.routing.push(RoutingRule {
            identity: "typoed".to_string(),
            default_agent: "librarian".to_string(),
            btw_agent: None,
            allowed_agents: vec!["missing-agent".to_string()],
        });
        let h = CommandHandler::new(Arc::new(config));

        let err = h.agent_choices_for_identity("typoed").unwrap_err();
        assert!(matches!(
            err,
            AgentChoiceError::UnknownAllowedAgents {
                identity_id,
                unknown_agents
            } if identity_id == "typoed" && unknown_agents == vec!["missing-agent"]
        ));
    }

    #[test]
    fn agent_choices_return_allowed_display_labels() {
        let h = make_handler();
        let choices = h.agent_choices_for_identity("david").unwrap();
        assert_eq!(
            choices,
            vec![("librarian".to_string(), "Librarian".to_string())]
        );
    }

    #[test]
    fn test_agents_shows_model_when_set() {
        let mut config = make_config();
        // Set a specific model on the librarian agent
        if let Some(agent) = config.agents.iter_mut().find(|a| a.id == "librarian") {
            agent.model = Some("claude-sonnet-4-6".to_string());
        }
        let h = CommandHandler::new(Arc::new(config));
        let reply = h.handle("!agents").unwrap();
        assert!(
            reply.contains("claude-sonnet-4-6"),
            "should show configured model: {}",
            reply
        );
    }

    #[test]
    fn test_model_command_lists_gateway_selector_classes() {
        let h = make_handler_with_synthetics();
        let reply = h.handle("!model").unwrap();
        assert!(reply.contains("Configured alloys:"), "{reply}");
        assert!(reply.contains("alloy-test"), "{reply}");
        assert!(reply.contains("Configured cascades:"), "{reply}");
        assert!(reply.contains("cascade-test"), "{reply}");
        assert!(reply.contains("Configured dispatchers:"), "{reply}");
        assert!(reply.contains("dispatcher-test"), "{reply}");
        assert!(!reply.contains("exec-backed model"), "{reply}");
    }

    #[test]
    fn test_model_command_activation_becomes_dispatch_override() {
        let h = make_handler_with_synthetics();
        let reply = h.handle_model("!model dispatcher-test", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_command_fails_cleanly_when_active_agent_ignores_model_overrides() {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let h = CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager());

        let reply = h.handle_model("!model dispatcher-test", "brian");

        assert!(
            reply.contains("does not consume Calciforge model overrides"),
            "reply should explain the active agent mismatch: {reply}"
        );
        assert_eq!(h.active_model_for_identity("brian"), None);
    }

    #[test]
    fn model_noun_aliases_list_and_activate() {
        let h = make_handler_with_synthetics();
        let list = h.handle("!model list").unwrap();
        assert!(list.contains("Configured dispatchers:"), "{list}");
        assert!(list.contains("!model use <id>"), "{list}");

        let reply = h.handle_model("!model use dispatcher-test", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_shortcut_alias_activates_synthetic_target() {
        let mut config = make_config();
        config.model_shortcuts.push(ModelShortcutConfig {
            alias: "fast".to_string(),
            model: "dispatcher-test".to_string(),
        });
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let h = CommandHandler::with_state_dir(Arc::new(config), tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager());
        h.handle_switch("!switch gateway", "brian");

        assert!(
            h.handle("!model fast").is_none(),
            "shortcut activation should be handled after identity resolution"
        );
        let reply = h.handle_model("!model fast", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");
        assert!(reply.contains("via alias 'fast'"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_role_alias_lists_and_activates_synthetic_target() {
        let mut config = make_config();
        config.model_roles.push(ModelRoleConfig {
            role: "security.screening".to_string(),
            model: "dispatcher-test".to_string(),
            description: Some("security scan role".to_string()),
        });
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let h = CommandHandler::with_state_dir(Arc::new(config), tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager());
        h.handle_switch("!switch gateway", "brian");

        let choices = h.activatable_model_choices();
        assert!(
            choices
                .iter()
                .any(|(id, label)| id == "security.screening" && label.contains("dispatcher-test")),
            "model role should be listed as an activatable model choice: {choices:?}"
        );
        let list = h.handle("!model list").unwrap();
        assert!(
            list.contains("security.screening → dispatcher-test"),
            "model role should appear in !model list: {list}"
        );

        let reply = h.handle_model("!model security.screening", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");
        assert!(reply.contains("via alias 'security.screening'"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_shortcut_alias_chain_activates_synthetic_target() {
        let mut config = make_config();
        config.model_shortcuts.push(ModelShortcutConfig {
            alias: "fast".to_string(),
            model: "local-dispatcher".to_string(),
        });
        config.model_shortcuts.push(ModelShortcutConfig {
            alias: "local-dispatcher".to_string(),
            model: "dispatcher-test".to_string(),
        });
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let h = CommandHandler::with_state_dir(Arc::new(config), tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager());
        h.handle_switch("!switch gateway", "brian");

        let reply = h.handle_model("!model fast", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");
        assert!(reply.contains("via alias 'fast'"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_shortcut_alias_activates_provider_target_and_is_choice() {
        let mut config = make_config();
        config.model_shortcuts.push(ModelShortcutConfig {
            alias: "premium".to_string(),
            model: "openai/gpt-5.5".to_string(),
        });
        let proxy = config.proxy.get_or_insert_with(Default::default);
        proxy.providers.push(crate::config::ProxyProviderConfig {
            id: "helicone".to_string(),
            backend_type: "http".to_string(),
            url: "http://127.0.0.1:1/v1".to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["openai/gpt-5.5".to_string()],
            strip_model_prefix: None,
            add_model_prefix: None,
            timeout_seconds: None,
            headers: HashMap::new(),
            on_switch: None,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        });
        let h = CommandHandler::new(Arc::new(config));
        h.handle_switch("!switch gateway", "brian");

        let choices = h.activatable_model_choices();
        assert!(
            choices
                .iter()
                .any(|(id, label)| id == "premium" && label.contains("openai/gpt-5.5")),
            "shortcut should be available as an activatable model choice: {choices:?}"
        );
        let reply = h.handle_model("!model use premium", "brian");
        assert!(reply.contains("Activated model"), "{reply}");
        assert!(reply.contains("via alias 'premium'"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("openai/gpt-5.5")
        );
    }

    #[test]
    fn test_model_command_activation_persists_across_handlers() {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let state_dir = tmp.path().to_path_buf();

        let h = CommandHandler::with_state_dir(config.clone(), state_dir.clone())
            .with_alloy_manager(synthetic_manager());
        h.handle_switch("!switch gateway", "brian");
        let reply = h.handle_model("!model dispatcher-test", "brian");
        assert!(reply.contains("Activated dispatcher"), "{reply}");

        let restored = CommandHandler::with_state_dir(config, state_dir)
            .with_alloy_manager(synthetic_manager());
        assert_eq!(
            restored.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn provider_model_activation_becomes_dispatch_override_and_survives_alloy_manager() {
        let mut config = make_config();
        let proxy = config.proxy.get_or_insert_with(Default::default);
        proxy.providers.push(crate::config::ProxyProviderConfig {
            id: "helicone".to_string(),
            backend_type: "http".to_string(),
            url: "http://127.0.0.1:1/v1".to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["openai/gpt-5.5".to_string()],
            strip_model_prefix: None,
            add_model_prefix: None,
            timeout_seconds: None,
            headers: HashMap::new(),
            on_switch: None,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        });
        let config = Arc::new(config);
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let state_dir = tmp.path().to_path_buf();

        let h = CommandHandler::with_state_dir(config.clone(), state_dir.clone())
            .with_alloy_manager(synthetic_manager());
        h.handle_switch("!switch gateway", "brian");
        let reply = h.handle_model("!model use openai/gpt-5.5", "brian");
        assert!(reply.contains("Activated model"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("openai/gpt-5.5")
        );

        let restored = CommandHandler::with_state_dir(config, state_dir)
            .with_alloy_manager(synthetic_manager());
        assert_eq!(
            restored.active_model_for_identity("brian").as_deref(),
            Some("openai/gpt-5.5"),
            "non-synthetic provider model overrides must not be discarded when the synthetic manager initializes"
        );
    }

    #[test]
    fn provider_model_activation_with_on_switch_waits_for_gateway_request() {
        let mut config = make_config();
        let proxy = config.proxy.get_or_insert_with(Default::default);
        proxy.providers.push(crate::config::ProxyProviderConfig {
            id: "helicone-ollama".to_string(),
            backend_type: "helicone".to_string(),
            url: "http://127.0.0.1:1/ai".to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["qwen3.6:27b".to_string()],
            strip_model_prefix: None,
            add_model_prefix: Some("ollama/".to_string()),
            timeout_seconds: None,
            headers: HashMap::new(),
            on_switch: Some("exit 99".to_string()),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        });
        let h = CommandHandler::new(Arc::new(config));
        h.handle_switch("!switch gateway", "brian");

        let reply = h.handle_model("!model use qwen3.6:27b", "brian");

        assert!(reply.contains("Activated model"), "{reply}");
        assert!(
            reply.contains("before the next gateway request"),
            "reply should explain that provider hooks run request-time, not command-time: {reply}"
        );
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("qwen3.6:27b")
        );
    }

    #[test]
    fn stale_active_model_override_is_pruned_on_restore() {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let state_dir = tmp.path().to_path_buf();
        let mut stale = HashMap::new();
        stale.insert("brian".to_string(), "openai/gpt-5.5".to_string());
        save_active_models_to(&state_dir, &stale);

        let h = CommandHandler::with_state_dir(config, state_dir.clone())
            .with_alloy_manager(synthetic_manager());

        assert_eq!(
            h.active_model_for_identity("brian"),
            None,
            "persisted model overrides must be dropped when the selector is no longer configured"
        );
        assert_eq!(
            load_active_models_from(&state_dir).get("brian"),
            None,
            "pruned model overrides must be removed from disk so doctor and service state agree"
        );
    }

    #[tokio::test]
    async fn test_status_shows_active_model_override() {
        let h = make_handler_with_synthetics();
        h.handle_switch("!switch gateway", "brian");
        h.handle_model("!model dispatcher-test", "brian");
        let reply = h.cmd_status_for_identity("brian").await;
        assert!(
            reply.contains("active model override: dispatcher-test"),
            "status should show active model override: {}",
            reply
        );
    }

    #[tokio::test]
    async fn test_status_shows_per_agent_model_summary() {
        let h = make_handler();
        let reply = h.cmd_status_for_identity("brian").await;
        // Both agents should appear in the agents summary line with their model (default since none set)
        assert!(
            reply.contains("librarian (default)"),
            "should show librarian with model: {}",
            reply
        );
        assert!(
            reply.contains("custodian (default)"),
            "should show custodian with model: {}",
            reply
        );
    }

    #[test]
    fn test_agents_empty_config() {
        let config = Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![],
            agents: vec![],
            routing: vec![],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            model_roles: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            security: None,
            proxy: None,
            local_models: None,
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

    #[tokio::test]
    async fn test_commands_case_insensitive() {
        let h = make_handler();
        assert_eq!(h.handle("!PING"), Some("pong".to_string()));
        assert_eq!(h.handle("!Ping"), Some("pong".to_string()));
        assert!(h.handle("!HELP").is_some());
        // !STATUS now requires identity context — returns None from handle()
        assert!(h.handle("!STATUS").is_none());
        // cmd_status_for_identity is case-insensitive at the identity level
        assert!(
            h.cmd_status_for_identity("brian")
                .await
                .contains("version:")
        );
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
    fn test_switch_is_not_handled_by_identity_independent_helper() {
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
        assert!(CommandHandler::is_switch_command("!agent switch librarian"));
        assert!(!CommandHandler::is_switch_command("!ping"));
        assert!(!CommandHandler::is_switch_command("!help"));
        assert!(!CommandHandler::is_switch_command("switch custodian")); // no !
        assert!(!CommandHandler::is_switch_command("hello world"));
    }

    #[test]
    fn test_agent_alias_for_switch() {
        let h = make_handler();
        // !agent is the alias for !switch — must behave identically:
        // returns None from handle() (needs auth first) and is recognized
        // by is_switch_command so the caller knows to route through auth.
        assert!(h.handle("!agent custodian").is_none());
        assert!(h.handle("!AGENT custodian").is_none());
        assert!(CommandHandler::is_switch_command("!agent custodian"));
        assert!(CommandHandler::is_switch_command("  !AGENT custodian  "));
    }

    #[test]
    fn test_switch_updates_active_agent_for_identity() {
        let h = make_handler();
        // Default is librarian
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));

        // Switch to custodian
        let reply = h.handle_switch("!switch custodian", "brian");
        assert!(
            reply.contains("custodian"),
            "reply should mention the agent: {}",
            reply
        );
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
        assert!(
            reply.contains("custodian"),
            "should mention the rejected agent: {}",
            reply
        );
        // Active agent should NOT have changed
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    #[test]
    fn test_switch_rejects_unknown_agent_with_valid_options() {
        let h = make_handler();
        let reply = h.handle_switch("!switch nonexistent", "brian");
        assert!(reply.contains("⚠️"), "should be a rejection: {}", reply);
        assert!(
            reply.contains("nonexistent"),
            "should mention the requested agent: {}",
            reply
        );
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
        assert!(
            reply.to_lowercase().contains("usage") || reply.contains("!switch"),
            "should show usage: {}",
            reply
        );
    }

    #[test]
    fn test_switch_case_insensitive_agent_name() {
        let h = make_handler();
        // "CUSTODIAN" should match "custodian"
        let reply = h.handle_switch("!switch CUSTODIAN", "brian");
        assert!(
            reply.contains('✅'),
            "case-insensitive switch should succeed: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_shows_display_name_in_reply() {
        let h = make_handler();
        // librarian has display_name = "Librarian" in registry
        let reply = h.handle_switch("!switch librarian", "brian");
        assert!(
            reply.contains("Librarian"),
            "should show display name: {}",
            reply
        );
    }

    #[test]
    fn test_switch_discloses_recent_context_sharing_when_enabled() {
        let h = make_handler();
        let reply = h.handle_switch("!switch librarian", "brian");
        assert!(
            reply.contains(
                "Context: recent thread context is shared with switched agents (up to 5 exchanges)."
            ),
            "switch reply should disclose context sharing mode: {reply}"
        );
    }

    #[test]
    fn test_switch_discloses_isolated_context_when_injection_disabled() {
        let mut config = make_config();
        config.context.inject_depth = 0;
        let h = make_handler_with_config(Arc::new(config));

        let reply = h.handle_switch("!switch librarian", "brian");
        assert!(
            reply.contains("Context: isolated; no prior thread context will be shared."),
            "switch reply should disclose isolated context mode: {reply}"
        );
    }

    #[test]
    fn test_switch_no_routing_rule_for_identity() {
        let h = make_handler();
        let reply = h.handle_switch("!switch librarian", "unknown_identity");
        assert!(
            reply.contains("⚠️"),
            "should reject unknown identity: {}",
            reply
        );
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
        assert!(
            reply.contains('✅'),
            "alias switch should succeed: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn agent_switch_noun_alias_succeeds() {
        let h = make_handler();
        let reply = h.handle_switch("!agent switch keeper", "brian");
        assert!(
            reply.contains('✅') && reply.contains("custodian"),
            "noun-style alias switch should succeed: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[tokio::test]
    async fn noun_alias_argument_parsing_accepts_tabs() {
        let h = make_handler_with_synthetics();

        let switch_reply = h.handle_switch("!agent\tswitch\tkeeper", "brian");
        assert!(
            switch_reply.contains('✅') && switch_reply.contains("custodian"),
            "tab-separated agent switch should succeed: {switch_reply}"
        );
        h.handle_switch("!agent switch gateway", "brian");

        let model_reply = h.handle_model("!model\tuse\tdispatcher-test", "brian");
        assert!(
            model_reply.contains("Activated dispatcher"),
            "tab-separated model use should succeed: {model_reply}"
        );

        let sessions = h.handle_sessions("!session\tlist\tcodex", "brian").await;
        assert!(
            sessions.contains("codex") && sessions.contains("supports named sessions"),
            "tab-separated session list should parse the agent argument: {sessions}"
        );
    }

    #[test]
    fn test_switch_by_alias_case_insensitive() {
        let h = make_handler();
        let reply = h.handle_switch("!switch CUST", "brian");
        assert!(
            reply.contains('✅'),
            "case-insensitive alias switch should succeed: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("custodian".to_string()));
    }

    #[test]
    fn test_switch_records_acpx_session_selection() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "acpx");
        write_test_executable(bin_dir.path(), "claude");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        let reply = h.handle_switch("!switch claude-acpx backend", "brian");
        assert!(
            reply.contains("session: backend"),
            "reply should identify selected session: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("claude-acpx".to_string()));
        assert_eq!(
            h.active_session_for("brian", "claude-acpx"),
            Some("backend".to_string())
        );
    }

    #[test]
    fn test_switch_records_named_session_for_session_capable_cli_agent() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "codex");
        let mut config = make_config();
        set_agent_path(&mut config, "codex", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        let reply = h.handle_switch("!switch codex work-thread", "brian");
        assert!(
            reply.contains("session: work-thread"),
            "reply should identify selected session for codex-cli: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("codex".to_string()));
        assert_eq!(
            h.active_session_for("brian", "codex"),
            Some("work-thread".to_string())
        );
    }

    #[test]
    fn test_new_session_sets_named_session_for_openclaw_channel_agent() {
        let h = make_handler();
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("Started session 'scratch' for librarian"),
            "default openclaw-channel test agent should accept !new: {reply}"
        );
        assert_eq!(
            h.active_session_for("brian", "librarian"),
            Some("scratch".to_string())
        );
    }

    #[test]
    fn test_new_session_requires_session_capable_active_agent() {
        let h = make_handler();
        h.handle_switch("!switch gateway", "brian");
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("does not expose downstream sessions"),
            "openai-compat agent should reject !new: {reply}"
        );
        assert_eq!(h.active_session_for("brian", "gateway"), None);
    }

    #[test]
    fn test_new_session_reports_missing_acpx_before_recording_session() {
        let empty_path = tempfile::tempdir().expect("empty path dir");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", empty_path.path());

        let h = make_handler_with_config(Arc::new(config));
        force_active_agent(&h, "brian", "claude-acpx");
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("Cannot start a session") && reply.contains("acpx executable"),
            "missing acpx should be reported before session is persisted: {reply}"
        );
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_new_session_reports_missing_acpx_agent_command_before_recording_session() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "acpx");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        force_active_agent(&h, "brian", "claude-acpx");
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("Cannot start a session")
                && reply.contains("configured ACPX downstream command 'claude'"),
            "missing acpx-managed agent command should be reported before session is persisted: {reply}"
        );
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_new_session_sets_named_session_for_current_agent() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "codex");
        let mut config = make_config();
        set_agent_path(&mut config, "codex", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        h.handle_switch("!switch codex", "brian");
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("Started session 'scratch' for codex"),
            "explicit !new session should be acknowledged: {reply}"
        );
        assert_eq!(
            h.active_session_for("brian", "codex"),
            Some("scratch".to_string())
        );
    }

    #[test]
    fn test_new_session_reports_missing_named_cli_command_before_recording_session() {
        let empty_path = tempfile::tempdir().expect("empty path dir");
        let mut config = make_config();
        set_agent_path(&mut config, "codex", empty_path.path());

        let h = make_handler_with_config(Arc::new(config));
        force_active_agent(&h, "brian", "codex");
        let reply = h.handle_new_session("!new scratch", "brian");
        assert!(
            reply.contains("Cannot start a session")
                && reply.contains("configured command 'codex'"),
            "missing named CLI command should be reported before session is persisted: {reply}"
        );
        assert_eq!(h.active_session_for("brian", "codex"), None);
    }

    #[test]
    fn test_switch_with_session_reports_missing_runtime_before_recording_session() {
        let empty_path = tempfile::tempdir().expect("empty path dir");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", empty_path.path());

        let h = make_handler_with_config(Arc::new(config));
        let reply = h.handle_switch("!switch claude-acpx backend", "brian");
        assert!(
            reply.contains("Cannot switch") && reply.contains("acpx executable"),
            "missing runtime should block session switch before state changes: {reply}"
        );
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_switch_acpx_without_session_clears_prior_session() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "acpx");
        write_test_executable(bin_dir.path(), "claude");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        h.handle_switch("!switch claude-acpx backend", "brian");
        assert_eq!(
            h.active_session_for("brian", "claude-acpx"),
            Some("backend".to_string())
        );

        let reply = h.handle_switch("!switch claude-acpx", "brian");
        assert!(
            reply.contains("default session"),
            "reply should show default session after clearing: {}",
            reply
        );
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_switch_acpx_rejects_path_like_session_name() {
        let h = make_handler();
        let reply = h.handle_switch("!switch claude-acpx ../backend", "brian");
        assert!(
            reply.contains("Invalid session name"),
            "path-like session should be rejected: {}",
            reply
        );
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_switch_acpx_rejects_multi_token_session_name_as_one_argument() {
        let h = make_handler();
        let reply = h.handle_switch("!switch claude-acpx backend session", "brian");
        assert!(
            reply.contains("Invalid session name"),
            "multi-token session should be rejected as one invalid session name: {}",
            reply
        );
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_default_clears_acpx_session_selection() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "acpx");
        write_test_executable(bin_dir.path(), "claude");
        let mut config = make_config();
        set_agent_path(&mut config, "claude-acpx", bin_dir.path());

        let h = make_handler_with_config(Arc::new(config));
        h.handle_switch("!switch claude-acpx backend", "brian");
        assert_eq!(
            h.active_session_for("brian", "claude-acpx"),
            Some("backend".to_string())
        );

        h.handle_default("brian");
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
        assert_eq!(h.active_session_for("brian", "claude-acpx"), None);
    }

    #[test]
    fn test_switch_alias_not_in_allowed_is_rejected() {
        let h = make_handler();
        // david is restricted to allowed_agents = ["librarian"]; "keeper" is custodian alias
        let reply = h.handle_switch("!switch keeper", "david");
        assert!(
            reply.contains("⚠️"),
            "alias outside allowed list must be rejected: {}",
            reply
        );
        assert_eq!(h.active_agent_for("david"), Some("librarian".to_string()));
    }

    #[test]
    fn parse_btw_uses_explicit_allowed_agent_without_switching_active_agent() {
        let h = make_handler();
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));

        let request = h
            .parse_btw_command("!btw code summarize this", "brian")
            .expect("explicit alias should resolve");

        assert_eq!(request.agent_id, "codex");
        assert_eq!(request.prompt, "summarize this");
        assert_eq!(
            h.active_agent_for("brian"),
            Some("librarian".to_string()),
            "!btw must not change active agent"
        );
    }

    #[test]
    fn parse_btw_uses_configured_default_agent_when_agent_omitted() {
        let mut config = make_config();
        config.routing[0].btw_agent = Some("codex".to_string());
        let tmp = tempfile::tempdir().expect("tempdir");
        let h = CommandHandler::with_state_dir(Arc::new(config), tmp.path().to_path_buf());

        let request = h
            .parse_btw_command("!btw quick one-off question", "brian")
            .expect("configured btw_agent should handle omitted target");

        assert_eq!(request.agent_id, "codex");
        assert_eq!(request.prompt, "quick one-off question");
    }

    #[test]
    fn parse_btw_rejects_unavailable_explicit_agent_for_identity() {
        let h = make_handler();
        let err = h
            .parse_btw_command("!btw keeper check this", "david")
            .unwrap_err();
        assert!(
            err.contains("not available"),
            "unavailable explicit target should not route through default fallback: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // !default command tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_command_not_handled_by_identity_independent_helper() {
        let h = make_handler();
        assert!(
            h.handle("!default").is_none(),
            "!default must return None from handle()"
        );
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
        assert!(
            reply.contains("librarian"),
            "reply should name the default agent: {}",
            reply
        );
        assert!(reply.contains('✅'), "should be a success reply: {}", reply);
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
    }

    #[test]
    fn test_default_is_idempotent_when_already_at_default() {
        let h = make_handler();
        // Already at librarian (the default) — !default should still succeed
        let reply = h.handle_default("brian");
        assert!(
            reply.contains('✅'),
            "!default from default should still succeed: {}",
            reply
        );
        assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()));
    }

    #[test]
    fn test_default_no_routing_rule_returns_error() {
        let h = make_handler();
        let reply = h.handle_default("unknown_identity");
        assert!(
            reply.contains("⚠️"),
            "unknown identity should get error: {}",
            reply
        );
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
        assert!(
            reply.contains("!default"),
            "help should mention !default: {}",
            reply
        );
    }

    // ── !secure tests ────────────────────────────────────────────────
    // These use the same fake-fnox-on-PATH trick as
    // `secrets-client/tests/vault_fallthrough.rs`: a temp dir holding a
    // shell script named `fnox` goes to the FRONT of PATH; that script
    // acts like fnox for the test's purposes. Real fnox presence on
    // the dev machine doesn't affect the result.

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static SECURE_ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn secure_set_command_detection_only_matches_value_entry() {
        assert!(CommandHandler::is_secure_set_command(
            "  !secure   set NAME=value"
        ));
        assert!(CommandHandler::is_secure_set_command(
            "  !secret   set NAME=value"
        ));
        assert!(CommandHandler::is_secure_set_command(
            "!SECURE SET NAME value"
        ));
        assert!(!CommandHandler::is_secure_set_command("!secure"));
        assert!(!CommandHandler::is_secure_set_command("!secure list"));
        assert!(!CommandHandler::is_secure_set_command("!secure help"));
        assert!(!CommandHandler::is_secure_set_command("!secret help"));
        assert!(!CommandHandler::is_secure_set_command("!status"));
    }

    #[test]
    fn secret_alias_is_secure_command() {
        assert!(CommandHandler::is_secure_command(
            "!secret input OPENAI_API_KEY"
        ));
        assert!(CommandHandler::is_secure_command("!SECRET list"));
        assert!(!CommandHandler::is_secure_command("!secrets"));
    }

    #[test]
    fn secure_help_sets_lan_expectations_for_paste_links() {
        let help = secure_help();

        assert!(
            help.contains("local-network"),
            "help should avoid implying the chat paste URL is localhost-only: {help}"
        );
        assert!(
            help.contains("LAN"),
            "help should tell users the browser must reach the Calciforge host: {help}"
        );
        assert!(
            help.contains("CALCIFORGE_PASTE_PUBLIC_BASE_URL"),
            "help should name the reverse-proxy/tunnel override: {help}"
        );
        assert!(
            help.contains("!secret bulk [desc]"),
            "bulk paste should not require an abstract label in chat help: {help}"
        );
        assert!(
            !help.contains("!secret bulk LABEL"),
            "chat help should not expose LABEL as a required concept: {help}"
        );
    }

    #[test]
    fn secure_bulk_uses_default_label_and_env_description() {
        let (label, description) = secure_input_target("", true).expect("bulk target");

        assert_eq!(label, "env-import");
        assert!(
            description.contains("KEY=VALUE"),
            "default bulk description should explain .env semantics: {description}"
        );
    }

    #[test]
    fn secure_bulk_treats_remainder_as_description_not_required_label() {
        let (label, description) =
            secure_input_target("GitHub project secrets", true).expect("bulk target");

        assert_eq!(label, "env-import");
        assert_eq!(description, "GitHub project secrets");
    }

    #[test]
    fn paste_server_env_defaults_chat_paste_to_detected_lan_listener() {
        let env =
            paste_server_env_from_values(None, false, None, None, Some("192.0.2.23:0".to_string()));

        assert_eq!(
            env,
            PasteServerEnv {
                bind: Some("192.0.2.23:0".to_string()),
                public_base_url: None,
                public_host: None,
            }
        );
    }

    #[test]
    fn paste_server_env_falls_back_to_paste_server_default_without_lan_detection() {
        let env = paste_server_env_from_values(None, false, None, None, None);

        assert_eq!(
            env,
            PasteServerEnv {
                bind: None,
                public_base_url: None,
                public_host: None,
            }
        );
    }

    #[test]
    fn paste_server_env_respects_explicit_bind_and_public_url() {
        let env = paste_server_env_from_values(
            Some("127.0.0.1:58083".to_string()),
            true,
            Some("https://calciforge.example.net/paste".to_string()),
            Some("calciforge.local".to_string()),
            Some("192.0.2.23:0".to_string()),
        );

        assert_eq!(
            env,
            PasteServerEnv {
                bind: Some("127.0.0.1:58083".to_string()),
                public_base_url: Some("https://calciforge.example.net/paste".to_string()),
                public_host: Some("calciforge.local".to_string()),
            }
        );
    }

    #[test]
    fn paste_server_env_does_not_override_inherited_paste_bind() {
        let env =
            paste_server_env_from_values(None, true, None, None, Some("192.0.2.23:0".to_string()));

        assert_eq!(
            env,
            PasteServerEnv {
                bind: None,
                public_base_url: None,
                public_host: None,
            }
        );
    }

    fn install_fake_fnox(dir: &TempDir, body: &str) -> std::path::PathBuf {
        let bin = dir.path().join("fnox");
        fs::write(&bin, format!("#!/bin/sh\n{body}\n")).expect("write fake fnox");
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).expect("chmod fake fnox");
        dir.path().to_path_buf()
    }

    struct PathGuard {
        original: Option<String>,
    }
    impl PathGuard {
        fn prepend(dir: &std::path::Path) -> Self {
            let original = std::env::var("PATH").ok();
            let new_path = match &original {
                Some(p) => format!("{}:{}", dir.display(), p),
                None => dir.display().to_string(),
            };
            // Safety: tests holding SECURE_ENV_MUTEX serialize env
            // mutation. `std::env::set_var` is marked unsafe in
            // Rust 2024 for this exact reason.
            unsafe {
                std::env::set_var("PATH", new_path);
            }
            Self { original }
        }
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(p) => std::env::set_var("PATH", p),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    /// Given a fake fnox that succeeds silently,
    /// when `handle_secure("!secure set NAME=value", ...)` runs,
    /// then the reply confirms storage using the NAME but NOT the value.
    ///
    /// Catches the core contract: a reply that accidentally echoed the
    /// value would render the command useless (value is already in the
    /// chat transport; echoing it makes it obvious to anyone reading
    /// the bot's output logs too).
    #[tokio::test]
    async fn secure_set_reply_includes_name_but_not_value() {
        let _lock = SECURE_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let fake_dir = install_fake_fnox(&dir, "exit 0");
        let _path = PathGuard::prepend(&fake_dir);

        let h = make_handler();
        let reply = h
            .handle_secure("!secure set MY_KEY=supersecretvalue", "brian")
            .await;

        assert!(
            reply.contains("MY_KEY"),
            "success reply should name the stored secret: {reply}"
        );
        assert!(
            !reply.contains("supersecretvalue"),
            "success reply must NOT echo the value: {reply}"
        );
    }

    /// Given a fake fnox that returns an error,
    /// when handle_secure runs,
    /// then the reply contains the fnox error text so the user can
    /// diagnose (config missing, provider broken, etc.), but still
    /// doesn't include the raw value.
    #[tokio::test]
    async fn secure_set_surfaces_fnox_error_without_echoing_value() {
        let _lock = SECURE_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let fake_dir = install_fake_fnox(&dir, r#"echo "No providers configured" >&2; exit 3"#);
        let _path = PathGuard::prepend(&fake_dir);

        let h = make_handler();
        let reply = h
            .handle_secure("!secure set ROT_KEY=rottenvalue", "brian")
            .await;

        assert!(reply.contains("failed") || reply.contains("⚠️"));
        // Error messages are allowed to name the stored key (users need
        // to know which set failed) but must still not echo the value.
        assert!(
            !reply.contains("rottenvalue"),
            "error reply must NOT echo value: {reply}"
        );
    }

    /// Given text that looks like `!secure` with a bad subcommand,
    /// when handle_secure runs,
    /// then the reply is the usage string, not a silent no-op, and
    /// does not shell out to fnox.
    #[tokio::test]
    async fn secure_unknown_subcommand_returns_help() {
        let _lock = SECURE_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Fake fnox that would fail if invoked — lets us assert the
        // handler never reaches it.
        let dir = TempDir::new().unwrap();
        let fake_dir = install_fake_fnox(&dir, "exit 42");
        let _path = PathGuard::prepend(&fake_dir);

        let h = make_handler();
        let reply = h.handle_secure("!secure bogus", "brian").await;

        assert!(reply.to_lowercase().contains("unknown"));
        assert!(reply.contains("!secure set") || reply.contains("subcommand"));
    }

    /// Given a `!secure set` with a name containing invalid chars
    /// (space, slash, dot),
    /// when handle_secure runs,
    /// then the reply rejects the name and doesn't shell out. Invalid
    /// names would otherwise produce silent fnox failures or collide
    /// with unexpected storage keys.
    #[tokio::test]
    async fn secure_set_rejects_invalid_name_chars() {
        let _lock = SECURE_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Fake fnox that returns success — if the handler wrongly
        // allowed the bad name through, the test would pass silently
        // instead of catching the rejection.
        let dir = TempDir::new().unwrap();
        let fake_dir = install_fake_fnox(&dir, "exit 0");
        let _path = PathGuard::prepend(&fake_dir);

        let h = make_handler();
        for bad in ["FOO BAR", "FOO/BAR", "FOO.BAR"] {
            let reply = h
                .handle_secure(&format!("!secure set {bad}=value"), "brian")
                .await;
            assert!(
                reply.contains("Invalid"),
                "invalid name {bad:?} should be rejected, got: {reply}"
            );
        }
    }

    /// Given a fake fnox that emits one name per line on `list`,
    /// when handle_secure("!secure list", …) runs,
    /// then the reply lists the names and does NOT echo any value.
    #[tokio::test]
    async fn secure_list_returns_names_only() {
        let _lock = SECURE_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        // Fake fnox list output: three names with some extra columns
        // that look like values — we must not surface those.
        let fake_dir = install_fake_fnox(
            &dir,
            r#"cat <<OUT
API_ONE  redacted-value-A
API_TWO  redacted-value-B
API_THREE redacted-value-C
OUT"#,
        );
        let _path = PathGuard::prepend(&fake_dir);

        let h = make_handler();
        let reply = h.handle_secure("!secure list", "brian").await;

        for name in ["API_ONE", "API_TWO", "API_THREE"] {
            assert!(
                reply.contains(name),
                "list reply should contain name {name:?}: {reply}"
            );
        }
        for leak in ["redacted-value-A", "redacted-value-B", "redacted-value-C"] {
            assert!(
                !reply.contains(leak),
                "list reply must NOT echo {leak:?}: {reply}"
            );
        }
    }
}
