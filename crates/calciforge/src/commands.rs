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

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::sync::{Arc, AtomicU64, Mutex, Ordering};

use crate::adapters::openclaw::{SharedPendingApprovals, ZeroClawHttpAdapter};
use crate::config::{calciforge_config_home, CalciforgeConfig};
use crate::messages::{ChoiceControl, ChoiceOption, OutboundMessage};
use crate::providers::alloy::AlloyManager;

/// Default state directory: `~/.config/calciforge/state/`.
fn default_state_dir() -> PathBuf {
    calciforge_config_home(None).join("state")
}

/// Path to the active-agent state file within `state_dir`.
fn state_file_path_for(state_dir: &Path) -> PathBuf {
    state_dir.join("active-agents.json")
}

/// Path to the active-model override state file within `state_dir`.
fn active_model_state_file_path_for(state_dir: &Path) -> PathBuf {
    state_dir.join("active-models.json")
}

/// Path to the active downstream session selections within `state_dir`.
fn active_session_state_file_path_for(state_dir: &Path) -> PathBuf {
    state_dir.join("active-agent-sessions.json")
}

fn first_arg(text: &str) -> Option<&str> {
    text.split_whitespace().nth(1)
}

fn second_arg(text: &str) -> Option<&str> {
    text.split_whitespace().nth(2)
}

fn command_token(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
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

fn command_suggestion(cmd: &str) -> Option<&'static str> {
    const MAX_FUZZY_COMMAND_CHARS: usize = 64;
    const COMMANDS: &[&str] = &[
        "!help",
        "!status",
        "!agents",
        "!agent",
        "!sessions",
        "!session",
        "!metrics",
        "!ping",
        "!switch",
        "!default",
        "!model",
        "!secure",
        "!secret",
        "!approve",
        "!deny",
    ];

    let lower = cmd.to_lowercase();
    let without_bang = lower.trim_start_matches('!');
    if without_bang.chars().count() > MAX_FUZZY_COMMAND_CHARS {
        return None;
    }

    COMMANDS
        .iter()
        .copied()
        .find(|candidate| candidate.trim_start_matches('!') == without_bang)
        .or_else(|| {
            COMMANDS.iter().copied().find(|candidate| {
                levenshtein_distance(without_bang, candidate.trim_start_matches('!')) <= 2
            })
        })
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let b_len = b.chars().count();
    let mut costs: Vec<usize> = (0..=b_len).collect();

    for (i, ca) in a.chars().enumerate() {
        let mut previous = costs[0];
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let insertion = costs[j + 1] + 1;
            let deletion = costs[j] + 1;
            let substitution = previous + usize::from(ca != cb);
            previous = costs[j + 1];
            costs[j + 1] = insertion.min(deletion).min(substitution);
        }
    }

    costs[b_len]
}

/// Load persisted active-agent selections from a given state directory.
/// Returns an empty map if the file doesn't exist or can't be parsed.
fn load_active_agents_from(state_dir: &Path) -> HashMap<String, String> {
    let path = state_file_path_for(state_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Load persisted active synthetic model selections from a given state directory.
/// Returns an empty map if the file doesn't exist or can't be parsed.
fn load_active_models_from(state_dir: &Path) -> HashMap<String, String> {
    let path = active_model_state_file_path_for(state_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Load persisted active downstream session selections.
/// Returns an empty map if the file doesn't exist or can't be parsed.
fn load_active_sessions_from(state_dir: &Path) -> HashMap<String, HashMap<String, String>> {
    let path = active_session_state_file_path_for(state_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Persist the active-agent map to a given state directory.
fn save_active_agents_to(state_dir: &Path, map: &HashMap<String, String>) {
    let path = state_file_path_for(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(&path, json);
    }
}

/// Persist the active synthetic model map to a given state directory.
fn save_active_models_to(state_dir: &Path, map: &HashMap<String, String>) {
    let path = active_model_state_file_path_for(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(&path, json);
    }
}

/// Persist the active downstream session selections to a given state directory.
fn save_active_sessions_to(state_dir: &Path, map: &HashMap<String, HashMap<String, String>>) {
    let path = active_session_state_file_path_for(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(&path, json);
    }
}

fn valid_downstream_session_name(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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
    /// Per-identity active synthetic model: identity_id → synthetic model id.
    /// Persisted to `state_dir/active-models.json` and restored into the
    /// [`AlloyManager`] once it is attached.
    active_models: Mutex<HashMap<String, String>>,
    /// Per-identity, per-agent active downstream session selection.
    /// Persisted to `state_dir/active-agent-sessions.json`.
    active_sessions: Mutex<HashMap<String, HashMap<String, String>>>,
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
            state_dir,
            pending_approvals: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            http_client,
            alloy_manager: None,
            local_manager: None,
        }
    }

    /// Set the alloy manager for this command handler.
    pub fn with_alloy_manager(mut self, manager: AlloyManager) -> Self {
        let mut active_models = self.active_models.lock().unwrap();
        active_models.retain(|identity_id, model_id| {
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
                tracing::warn!(
                    identity = %identity_id,
                    model = %model_id,
                    "dropping persisted active-model selection for unknown synthetic model"
                );
                false
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

    /// Get a reference to the synthetic model manager, if configured.
    pub fn alloy_manager(&self) -> Option<&AlloyManager> {
        self.alloy_manager.as_ref()
    }

    /// Return the active synthetic model override for an identity, if one was selected.
    pub fn active_model_for_identity(&self, identity_id: &str) -> Option<String> {
        self.alloy_manager
            .as_ref()
            .and_then(|manager| manager.active_for_identity(identity_id))
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

    /// Return model choices that can be activated with `!model use <id>`.
    pub fn activatable_model_choices(&self) -> Vec<(String, String)> {
        let mut choices = Vec::new();
        if let Some(manager) = self.alloy_manager.as_ref() {
            choices.extend(
                manager
                    .list()
                    .into_iter()
                    .map(|model| (model.id.clone(), format!("{} (alloy)", model.name))),
            );
            choices.extend(
                manager
                    .list_cascades()
                    .into_iter()
                    .map(|model| (model.id.clone(), format!("{} (cascade)", model.name))),
            );
            choices.extend(
                manager
                    .list_dispatchers()
                    .into_iter()
                    .map(|model| (model.id.clone(), format!("{} (dispatcher)", model.name))),
            );
            choices.extend(
                manager
                    .list_exec_models()
                    .into_iter()
                    .map(|model| (model.id.clone(), format!("{} (exec)", model.name))),
            );
        }
        if let Some(manager) = self.local_manager.as_ref() {
            choices.extend(manager.models().iter().map(|model| {
                (
                    model.id.clone(),
                    model
                        .display_name
                        .clone()
                        .unwrap_or_else(|| format!("{} (local)", model.id)),
                )
            }));
        }
        choices.sort_by(|left, right| left.0.cmp(&right.0));
        choices
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

    /// Returns `true` for model list commands that can include activatable choices.
    pub fn is_model_choice_request(text: &str) -> bool {
        let mut tokens = text.split_whitespace();
        let Some(cmd) = tokens.next() else {
            return false;
        };
        let sub = tokens.next();
        if tokens.next().is_some() {
            return false;
        }

        match sub {
            None => cmd.eq_ignore_ascii_case("!model"),
            Some(sub) => {
                cmd.eq_ignore_ascii_case("!model")
                    && (sub.eq_ignore_ascii_case("list")
                        || sub.eq_ignore_ascii_case("ls")
                        || sub.eq_ignore_ascii_case("models"))
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

        let base_reply = self
            .handle(text)
            .unwrap_or_else(|| "Configured agents unavailable.".to_string());
        match self.agent_choices_for_identity(identity_id) {
            Ok(choices) => Some(
                OutboundMessage::text(base_reply).with_control(ChoiceControl::new(
                    "Choose an agent",
                    choices
                        .into_iter()
                        .map(|(id, label)| ChoiceOption::agent(label, id))
                        .collect(),
                )),
            ),
            Err(err) => Some(OutboundMessage::text(format!(
                "{base_reply}\n\nButton choices unavailable: {err}."
            ))),
        }
    }

    /// Build a channel-agnostic model choice response.
    pub fn model_choice_message(&self, text: &str) -> Option<OutboundMessage> {
        if !Self::is_model_choice_request(text) {
            return None;
        }

        let choices = self.activatable_model_choices();
        let reply = self.handle(text).unwrap_or_else(|| {
            if choices.is_empty() {
                "No activatable model choices are configured. Type `!model` for configured shortcuts."
                    .to_string()
            } else {
                "Choose a model, or type `!model use <id>`:".to_string()
            }
        });

        let options = choices
            .into_iter()
            .map(|(id, label)| ChoiceOption::model(label, id))
            .collect::<Vec<_>>();
        Some(
            OutboundMessage::text(reply)
                .with_control(ChoiceControl::new("Choose a model", options)),
        )
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

    /// Return the currently selected downstream session for an identity/agent.
    pub fn active_session_for(&self, identity_id: &str, agent_id: &str) -> Option<String> {
        let map = self.active_sessions.lock().unwrap();
        map.get(identity_id)
            .and_then(|sessions| sessions.get(agent_id))
            .cloned()
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
        let cmd = command_token(trimmed).to_lowercase();

        match cmd.as_str() {
            "!help" => Some(self.cmd_help()),
            "!commands" => Some(self.cmd_help()),
            // !status needs auth — return None so the caller resolves identity first.
            "!status" => None,
            "!agents" => Some(self.cmd_agents_summary()),
            "!metrics" => Some(self.cmd_metrics()),
            "!ping" => Some("pong".to_string()),
            // !sessions needs auth — return None so caller resolves identity first.
            "!sessions" | "!session" => None,
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

    /// Return true if the text starts with '!' (a command).
    pub fn is_command(text: &str) -> bool {
        text.trim().starts_with('!')
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

    /// Handle a command that may require async work (approve/deny).
    ///
    /// Returns `Some((ack, Option<follow_up>))` if the text matches `!approve`
    /// or `!deny`, `None` if it is not a recognized async command.
    ///
    /// Callers should send `ack` immediately, then send `follow_up` (if present)
    /// once it arrives — it carries the continuation agent response after the
    /// approval/denial has been relayed to ZeroClaw and polled for a result.
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

    /// Build the operator-facing approval request with reusable approve/deny choices.
    pub fn approval_request_message(
        command: &str,
        reason: &str,
        request_id: &str,
    ) -> OutboundMessage {
        let text = format!(
            "Approval required\nCommand: {command}\nReason: {reason}\nRequest ID: {request_id}"
        );
        OutboundMessage::text(text).with_control(ChoiceControl::new(
            "Choose an approval action",
            vec![
                ChoiceOption::approve(request_id),
                ChoiceOption::deny(request_id),
            ],
        ))
    }

    /// Handle an `!approve [request_id]` command.
    ///
    /// If no `request_id` is provided and exactly one approval is pending,
    /// auto-selects it.  Signals ZeroClaw to allow the blocked tool call, then
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

        // Signal ZeroClaw to approve.
        match ZeroClawHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
            true,
            None,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (format!("⚠️ Failed to send approval: {e}"), None);
            }
        }

        // Remove from local pending store.
        self.pending_approvals.lock().await.remove(&meta.request_id);

        // Poll for the continuation result.
        let result = ZeroClawHttpAdapter::poll_result(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
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
    /// auto-selects it.  Signals ZeroClaw to deny the blocked tool call, then
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

        // Signal ZeroClaw to deny.
        match ZeroClawHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
            false,
            reason,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (format!("⚠️ Failed to send denial: {e}"), None);
            }
        }

        // Remove from local pending store.
        self.pending_approvals.lock().await.remove(&meta.request_id);

        // Poll for the continuation result.
        let result = ZeroClawHttpAdapter::poll_result(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
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

    /// Handle a `!switch <agent> [session]` command for an authenticated identity.
    ///
    /// Validates the requested agent against the identity's `allowed_agents`,
    /// updates the active-agent map, and returns a confirmation message.
    /// For acpx-type agents, an optional session name can be specified.
    ///
    /// Returns an error string (to be sent back to the user) on any validation
    /// failure — never panics.
    pub fn handle_switch(&self, text: &str, identity_id: &str) -> String {
        let trimmed = text.trim();
        // Parse arguments after "!switch" or noun-style "!agent switch".
        let mut parts = trimmed.split_whitespace();
        let is_agent_cmd = parts
            .next()
            .is_some_and(|cmd| cmd.eq_ignore_ascii_case("!agent"));
        let mut args: Vec<&str> = parts.collect();
        if is_agent_cmd
            && args.first().is_some_and(|arg| {
                arg.eq_ignore_ascii_case("switch") || arg.eq_ignore_ascii_case("use")
            })
        {
            args.remove(0);
        }

        if args.is_empty() {
            return "Usage: !switch <agent> [session]\nAlias: !agent switch <agent> [session]\n\nUse !agent list to see available agents.\nUse !session list <agent> to list available sessions for acpx agents.".to_string();
        }

        let agent_arg = args[0].to_string();
        let session_arg = (args.len() > 1).then(|| args[1..].join(" "));

        // Look up the routing rule for this identity.
        let routing_rule = match self
            .config
            .routing
            .iter()
            .find(|r| r.identity == identity_id)
        {
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
            routing_rule
                .allowed_agents
                .iter()
                .map(|s| s.as_str())
                .collect()
        };

        // Case-insensitive match of the requested agent against allowed list,
        // checking both agent id and any configured aliases.
        let matched_agent = allowed
            .iter()
            .find(|&&a| {
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
            })
            .copied();

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
                let agent_cfg = self.config.agents.iter().find(|a| a.id == agent_id);
                let display_name = agent_cfg
                    .and_then(|a| a.registry.as_ref())
                    .and_then(|r| r.display_name.as_deref())
                    .unwrap_or(agent_id);

                // Check if this is an acpx agent and session was specified
                let is_acpx = agent_cfg.map(|a| a.kind == "acpx").unwrap_or(false);
                if is_acpx {
                    if let Some(session) = session_arg.as_deref() {
                        if !valid_downstream_session_name(session) {
                            return "⚠️ Invalid session name. Use only letters, numbers, dot, underscore, and dash.".to_string();
                        }
                    }
                }
                let session_info = if is_acpx {
                    if let Some(session) = session_arg.as_ref() {
                        format!(" (session: {})", session)
                    } else {
                        " (default session)".to_string()
                    }
                } else if session_arg.is_some() {
                    " (note: session parameter ignored for non-acpx agents)".to_string()
                } else {
                    String::new()
                };

                // Update per-identity active agent and persist to disk.
                {
                    let mut map = self.active_agents.lock().unwrap();
                    map.insert(identity_id.to_string(), agent_id.to_string());
                    save_active_agents_to(&self.state_dir, &map);
                }
                {
                    let mut sessions = self.active_sessions.lock().unwrap();
                    if let Some(session) = session_arg.as_ref().filter(|_| is_acpx) {
                        sessions
                            .entry(identity_id.to_string())
                            .or_default()
                            .insert(agent_id.to_string(), session.to_string());
                    } else if is_acpx {
                        let mut remove_identity = false;
                        if let Some(identity_sessions) = sessions.get_mut(identity_id) {
                            identity_sessions.remove(agent_id);
                            remove_identity = identity_sessions.is_empty();
                        }
                        if remove_identity {
                            sessions.remove(identity_id);
                        }
                    }
                    save_active_sessions_to(&self.state_dir, &sessions);
                }

                format!(
                    "✅ Switched to {}{}. Your messages will now route to {}.",
                    display_name, session_info, agent_id
                )
            }
        }
    }

    #[allow(dead_code)]
    pub async fn handle_sessions(&self, text: &str, identity_id: &str) -> String {
        self.handle_sessions_message(text, identity_id)
            .await
            .render_text_fallback()
    }

    /// Handle a `!sessions` command for an authenticated identity.
    ///
    /// Lists ACP sessions for the specified agent (for acpx-type agents).
    /// Returns a channel-agnostic message with selectable session choices when
    /// the ACPX backend reports active sessions.
    pub async fn handle_sessions_message(&self, text: &str, identity_id: &str) -> OutboundMessage {
        let trimmed = text.trim();
        // Parse the agent argument after "!sessions", "!session", or
        // noun-style "!session list".
        let mut args: Vec<&str> = trimmed.split_whitespace().skip(1).collect();
        if args
            .first()
            .is_some_and(|arg| arg.eq_ignore_ascii_case("list") || arg.eq_ignore_ascii_case("show"))
        {
            args.remove(0);
        }
        let agent_arg = args.first().copied().unwrap_or("").to_string();

        if agent_arg.is_empty() {
            return OutboundMessage::text("Usage: !sessions <agent>\nAlias: !session list <agent>\n\nLists available ACP sessions for an agent.\nUse !agent list to see available agents.");
        }

        // Look up the routing rule for this identity.
        let routing_rule = match self
            .config
            .routing
            .iter()
            .find(|r| r.identity == identity_id)
        {
            Some(r) => r,
            None => {
                return OutboundMessage::text("⚠️ No routing rule found for your identity.");
            }
        };

        // Determine which agents this identity is allowed to use.
        let allowed: Vec<&str> = if routing_rule.allowed_agents.is_empty() {
            self.config.agents.iter().map(|a| a.id.as_str()).collect()
        } else {
            routing_rule
                .allowed_agents
                .iter()
                .map(|s| s.as_str())
                .collect()
        };

        // Find the matched agent (case-insensitive, checking aliases).
        let matched_agent = allowed
            .iter()
            .find(|&&a| {
                if a.eq_ignore_ascii_case(&agent_arg) {
                    return true;
                }
                if let Some(agent_cfg) = self.config.agents.iter().find(|ag| ag.id == a) {
                    return agent_cfg
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(&agent_arg));
                }
                false
            })
            .copied();

        let agent_id = match matched_agent {
            None => {
                let valid = allowed.join(", ");
                return OutboundMessage::text(format!(
                    "⚠️ Agent '{}' is not available to you.\n\nValid agents: {}",
                    agent_arg, valid
                ));
            }
            Some(id) => id,
        };

        // Get agent config to check if it's an acpx agent.
        let agent_cfg = match self.config.agents.iter().find(|a| a.id == agent_id) {
            Some(cfg) => cfg,
            None => {
                return OutboundMessage::text(format!(
                    "⚠️ Agent '{}' not found in configuration.",
                    agent_id
                ));
            }
        };

        if agent_cfg.kind != "acpx" {
            return OutboundMessage::text(format!(
                "ℹ️ Agent '{}' ({}) does not support session listing.\nOnly 'acpx' type agents support sessions.",
                agent_id, agent_cfg.kind
            ));
        }

        // List sessions using acpx.
        let agent_name = agent_cfg.command.as_deref().unwrap_or(agent_id);
        match self.list_acpx_sessions(agent_name).await {
            Ok(sessions) if sessions.is_empty() => {
                OutboundMessage::text(format!(
                    "ℹ️ No active sessions for '{}'.\n\nUse !switch {} to create a new session.",
                    agent_id, agent_id
                ))
            }
            Ok(sessions) => {
                active_sessions_message(agent_id, sessions)
            }
            Err(e) => {
                OutboundMessage::text(format!(
                    "⚠️ Failed to list sessions for '{}': {}\n\nMake sure acpx is installed and the agent is properly configured.",
                    agent_id, e
                ))
            }
        }
    }

    /// List ACPX sessions for an agent using the acpx CLI.
    async fn list_acpx_sessions(&self, agent_name: &str) -> Result<Vec<String>, String> {
        tokio::fs::create_dir_all(crate::adapters::acpx::ACPX_SESSION_DIR)
            .await
            .map_err(|e| format!("Failed to create acpx session dir: {}", e))?;

        let output = tokio::process::Command::new("acpx")
            .arg(agent_name)
            .arg("sessions")
            .arg("list")
            .current_dir(crate::adapters::acpx::ACPX_SESSION_DIR)
            .output()
            .await
            .map_err(|e| format!("Failed to run acpx: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("acpx error: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let sessions: Vec<String> = stdout
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("No sessions"))
            .map(|s| s.to_string())
            .collect();

        Ok(sessions)
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
            save_active_agents_to(&self.state_dir, &map);
        }
        {
            let mut sessions = self.active_sessions.lock().unwrap();
            sessions.remove(identity_id);
            save_active_sessions_to(&self.state_dir, &sessions);
        }

        format!("✅ Switched to default agent: {}", default_agent_id)
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

    fn cmd_help(&self) -> String {
        [
            "Calciforge — available commands:",
            "  !help, !commands — show this help",
            "  !status  — version, uptime, active agent, config summary",
            "  !agents  — list configured agents",
            "  !sessions <agent> — list ACP sessions for an agent (requires auth)",
            "  !metrics — messages routed, average latency",
            "  !ping    — connectivity check (replies: pong)",
            "  !switch, !agent <agent> [session] — switch active agent (requires auth)",
            "  !agent list | !agent details [agent] | !agent switch <agent> — noun-style agent commands",
            "  !default — switch back to your default agent (requires auth)",
            "  !model [list|use <id>|alias] — show or activate model choices",
            "  !secure, !secret <input|bulk|list|help> — paste URLs and secret names; `set` is legacy fallback",
            "  !approve [request_id] — approve a pending Clash tool call",
            "  !deny [request_id] [reason] — deny a pending Clash tool call",
        ]
        .join("\n")
    }
    /// Pre-auth handling for !model — lists shortcuts/alloys.
    /// Returns None if an alloy is being selected (requires post-auth handling).
    fn cmd_model_preauth(&self, text: &str) -> Option<String> {
        let mut args: Vec<&str> = text.split_whitespace().skip(1).collect();

        let list_requested = args.is_empty()
            || args.first().is_some_and(|arg| {
                arg.eq_ignore_ascii_case("list")
                    || arg.eq_ignore_ascii_case("ls")
                    || arg.eq_ignore_ascii_case("show")
            });

        if list_requested {
            // No argument — list all shortcuts and alloys
            let mut lines = vec![];

            // Model shortcuts section
            if !self.config.model_shortcuts.is_empty() {
                lines.push("Model shortcuts:".to_string());
                for shortcut in &self.config.model_shortcuts {
                    lines.push(format!("  {} → {}", shortcut.alias, shortcut.model));
                }
            }

            // Synthetic models section
            if let Some(ref manager) = self.alloy_manager {
                if !manager.is_empty() {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push("Configured alloys:".to_string());
                    for alloy in manager.list() {
                        let constituents: Vec<String> = alloy
                            .constituents
                            .iter()
                            .map(|c| format!("{} (weight {})", c.model, c.weight))
                            .collect();
                        lines.push(format!(
                            "  {} — {} ({:?}): {}",
                            alloy.id,
                            alloy.name,
                            alloy.strategy,
                            constituents.join(", ")
                        ));
                    }
                    let cascades = manager.list_cascades();
                    if !cascades.is_empty() {
                        lines.push(String::new());
                        lines.push("Configured cascades:".to_string());
                        for cascade in cascades {
                            let models: Vec<String> = cascade
                                .models
                                .iter()
                                .map(|m| format!("{} ({} tokens)", m.model, m.context_window))
                                .collect();
                            lines.push(format!(
                                "  {} — {}: {}",
                                cascade.id,
                                cascade.name,
                                models.join(" → ")
                            ));
                        }
                    }
                    let dispatchers = manager.list_dispatchers();
                    if !dispatchers.is_empty() {
                        lines.push(String::new());
                        lines.push("Configured dispatchers:".to_string());
                        for dispatcher in dispatchers {
                            let models: Vec<String> = dispatcher
                                .models
                                .iter()
                                .map(|m| format!("{} ({} tokens)", m.model, m.context_window))
                                .collect();
                            lines.push(format!(
                                "  {} — {}: {}",
                                dispatcher.id,
                                dispatcher.name,
                                models.join(", ")
                            ));
                        }
                    }
                    let exec_models = manager.list_exec_models();
                    if !exec_models.is_empty() {
                        lines.push(String::new());
                        lines.push("Configured exec models:".to_string());
                        for exec_model in exec_models {
                            lines.push(format!(
                                "  {} — {} ({} tokens)",
                                exec_model.id, exec_model.name, exec_model.context_window
                            ));
                        }
                    }
                }
            }

            if lines.is_empty() {
                return Some("No model shortcuts or synthetic models configured.\n\nAdd shortcuts to your config:\n[[model_shortcuts]]\nalias = \"sonnet\"\nmodel = \"anthropic/claude-sonnet-4.6\"".to_string());
            }

            lines.push("\nUsage:".to_string());
            lines.push("  !model list — show this list".to_string());
            lines.push("  !model <alias> — show model for alias".to_string());
            if self.alloy_manager.is_some() {
                lines.push(
                    "  !model use <synthetic-id> — activate alloy/cascade/dispatcher/exec model for your identity"
                        .to_string(),
                );
            }
            Some(lines.join("\n"))
        } else {
            // Argument provided — check if it's a model shortcut or synthetic selection
            if args.first().is_some_and(|arg| {
                arg.eq_ignore_ascii_case("use")
                    || arg.eq_ignore_ascii_case("switch")
                    || arg.eq_ignore_ascii_case("set")
            }) {
                args.remove(0);
            }
            let arg = args.first().copied().unwrap_or("");

            // First check if it's a model shortcut (show-only)
            if let Some(shortcut) = self.config.model_shortcuts.iter().find(|s| s.alias == arg) {
                return Some(format!("{} → {}", shortcut.alias, shortcut.model));
            }

            // If no alloy manager, unknown alias
            if self.alloy_manager.is_none() {
                return Some(format!(
                    "Unknown alias: '{}',\n\nUse !model to see available shortcuts.",
                    arg
                ));
            }

            // Return None to trigger post-auth handling for synthetic selection
            None
        }
    }

    /// Handle a `!model <id>` command for an authenticated identity.
    ///
    /// Dispatch order:
    /// 1. If the ID matches a configured synthetic model → activate it.
    /// 2. If the ID matches a local model in `[local_models]` → trigger a switch
    ///    (async background task, returns immediately with status message).
    /// 3. If a `[[proxy.providers]]` entry has an `on_switch` hook for this model
    ///    → run the hook script in the background.
    /// 4. Otherwise → show an error with available options.
    pub fn handle_model(&self, text: &str, identity_id: &str) -> String {
        let trimmed = text.trim();
        let mut args: Vec<&str> = trimmed.split_whitespace().skip(1).collect();
        if args.first().is_some_and(|arg| {
            arg.eq_ignore_ascii_case("use")
                || arg.eq_ignore_ascii_case("switch")
                || arg.eq_ignore_ascii_case("set")
        }) {
            args.remove(0);
        }

        if args.is_empty() {
            return "Usage: !model use <id>\nAlias: !model <id>\n\nUse !model list to see available models.".to_string();
        }

        let model_id = args[0];

        // 1. Synthetic model switch.
        if let Some(ref manager) = self.alloy_manager {
            if manager.is_synthetic_model(model_id) {
                if let Err(e) = manager.set_active_for_identity(identity_id, model_id) {
                    return format!("⚠️ Failed to activate model: {}", e);
                }
                {
                    let mut active_models = self.active_models.lock().unwrap();
                    active_models.insert(identity_id.to_string(), model_id.to_string());
                    save_active_models_to(&self.state_dir, &active_models);
                }
                if let Some(alloy) = manager.get(model_id) {
                    let constituents: Vec<String> = alloy
                        .definition()
                        .constituents
                        .iter()
                        .map(|c| format!("{} (weight {})", c.model, c.weight))
                        .collect();
                    return format!(
                        "✅ Activated alloy '{}' for your identity.\n\nConstituents ({:?} strategy): {}",
                        model_id,
                        alloy.definition().strategy,
                        constituents.join(", ")
                    );
                }
                return format!(
                    "✅ Activated synthetic model '{}' for your identity.",
                    model_id
                );
            }
        }

        // 2. Local model switch.
        if let Some(ref lm_mgr) = self.local_manager {
            if let Some(model_def) = lm_mgr.find_model(model_id) {
                let hf_id = model_def.hf_id.clone();
                let id = model_id.to_string();
                let mgr = crate::sync::Arc::clone(lm_mgr);
                // Run the blocking switch in a background task — may take 1-2 minutes.
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || mgr.switch(&id)).await;
                    match result {
                        Ok(Ok(loaded)) => {
                            tracing::info!(model = %loaded.id, "!model local switch complete");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "!model local switch failed");
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "!model local switch panic");
                        }
                    }
                });
                return format!(
                    "🔄 Switching to local model '{}' (HF: {}).\n\
                    This may take 1-2 minutes while the model loads.\n\
                    The gateway will continue serving requests during the transition.",
                    model_id, hf_id
                );
            }
        }

        // 3. Provider on_switch hook.
        if let Some(ref proxy_cfg) = self.config.proxy {
            for provider in &proxy_cfg.providers {
                let model_matches = provider
                    .models
                    .iter()
                    .any(|p| crate::proxy::routing::model_matches_pattern(model_id, p));
                if model_matches {
                    if let Some(ref hook_script) = provider.on_switch {
                        if !hook_script.is_empty() {
                            let script = hook_script.clone();
                            let model_id_owned = model_id.to_string();
                            let model_id_log = model_id.to_string();
                            let provider_id = provider.id.clone();
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    std::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(&script)
                                        .env("CALCIFORGE_MODEL_ID", &model_id_owned)
                                        .output()
                                })
                                .await;
                                match result {
                                    Ok(Ok(out)) if out.status.success() => {
                                        tracing::info!(provider = %provider_id, model = %model_id_log, "on_switch hook completed");
                                    }
                                    Ok(Ok(out)) => {
                                        tracing::warn!(
                                            provider = %provider_id,
                                            stderr = %String::from_utf8_lossy(&out.stderr),
                                            "on_switch hook failed"
                                        );
                                    }
                                    Ok(Err(e)) => {
                                        tracing::warn!(error = %e, "on_switch hook spawn error");
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "on_switch hook panic");
                                    }
                                }
                            });
                            return format!(
                                "🔄 Running on_switch hook for model '{}' (provider: {}).",
                                model_id, provider.id
                            );
                        }
                        // Provider matches but no hook — just acknowledge.
                        return format!(
                            "ℹ️ Model '{}' is served by provider '{}' (no on_switch hook configured).",
                            model_id, provider.id
                        );
                    }
                }
            }
        }

        // 4. Unknown model — show what's available.
        let mut available = vec![];
        if let Some(ref mgr) = self.alloy_manager {
            for a in mgr.list() {
                available.push(format!("  {} (alloy)", a.id));
            }
            for c in mgr.list_cascades() {
                available.push(format!("  {} (cascade)", c.id));
            }
            for d in mgr.list_dispatchers() {
                available.push(format!("  {} (dispatcher)", d.id));
            }
            for e in mgr.list_exec_models() {
                available.push(format!("  {} (exec)", e.id));
            }
        }
        if let Some(ref lm) = self.local_manager {
            for m in lm.models() {
                available.push(format!("  {} (local/{})", m.id, m.provider_type));
            }
        }
        if available.is_empty() {
            return format!("⚠️ Unknown model: '{}'\n\nNo models configured.", model_id);
        }
        format!(
            "⚠️ Unknown model: '{}'\n\nAvailable:\n{}",
            model_id,
            available.join("\n")
        )
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
// !secret / !secure subcommand implementations (free functions so tests can drive
// them without constructing a full CommandHandler).
// ---------------------------------------------------------------------------

fn secure_help() -> String {
    [
        "!secret subcommands (alias: !secure):",
        "  !secret input NAME [desc] — create a one-shot local-network paste link",
        "  !secret bulk [desc]     — create a one-shot local-network .env paste link",
        "  !secret list              — list stored secret names (not values)",
        "  !secret help              — show this help",
        "",
        "The input/bulk links are for browsers that can reach this Calciforge",
        "host on your LAN. For stable hostnames or off-LAN access, configure",
        "CALCIFORGE_PASTE_PUBLIC_BASE_URL behind an authenticated reverse proxy",
        "or tunnel; do not expose paste-server directly to the open internet.",
        "",
        "Equivalent host-local commands:",
        "  paste-server NAME \"description\"",
        "  paste-server --bulk env-import \"bulk .env import\"",
        "",
        "Legacy chat fallback:",
        "  !secret set NAME=value    — store a low-stakes secret by name",
        "  !secret set NAME value    — same, for mobile keyboards",
        "",
        "⚠️ `!secret set` passes through the chat transport, which retains",
        "   history and may not be end-to-end encrypted. `input`/`bulk` send",
        "   only a short-lived URL; the value is entered in the browser.",
        "",
        "Calciforge and fnox can share the same fnox.toml/profile. Installing",
        "fnox is still useful for manual `fnox set/list/tui` operations and as",
        "the default local secret backend for paste-server. On macOS the installer",
        "adds a Keychain provider; on Linux it creates a local age provider.",
    ]
    .join("\n")
}

async fn secure_input(rest: &str, bulk: bool) -> String {
    let (name_or_label, description) = match secure_input_target(rest, bulk) {
        Ok(target) => target,
        Err(usage) => {
            return usage;
        }
    };

    let mut command = tokio::process::Command::new("paste-server");
    if bulk {
        command.arg("--bulk");
    }
    configure_paste_server_env(&mut command);
    command
        .arg(name_or_label)
        .arg(description)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "⚠️ paste-server is not installed or not on the service PATH. Re-run the Calciforge installer or install the `paste-server` binary.".to_string();
        }
        Err(e) => return format!("⚠️ Failed to start paste-server: {e}"),
    };

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return "⚠️ paste-server started without a URL stream".to_string();
    };

    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        reader.read_line(&mut line),
    )
    .await;
    let url = match read_result {
        Ok(Ok(n)) if n > 0 => line.trim().to_string(),
        Ok(Ok(_)) => {
            let _ = child.kill().await;
            return "⚠️ paste-server exited before returning a URL".to_string();
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            return format!("⚠️ Failed reading paste-server URL: {e}");
        }
        Err(_) => {
            let _ = child.kill().await;
            return "⚠️ paste-server did not return a URL within 3 seconds".to_string();
        }
    };

    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    let mode = if bulk {
        "bulk .env paste"
    } else {
        "secret paste"
    };
    format!(
        "🔐 One-shot {mode} link:\n{url}\n\n\
         Open it from a browser that can reach the Calciforge host. The link expires quickly and stores through the configured local secret backend. \
         For phone/off-network use, use a short-lived authenticated proxy/tunnel; do not expose this paste server directly to the open internet."
    )
}

fn secure_input_target(rest: &str, bulk: bool) -> Result<(String, String), String> {
    if bulk {
        let description = rest.trim();
        return Ok((
            "env-import".to_string(),
            if description.is_empty() {
                "Paste .env lines; each KEY=VALUE line becomes its own secret.".to_string()
            } else {
                description.to_string()
            },
        ));
    }

    let mut parts = rest.split_whitespace();
    let Some(name) = parts.next() else {
        return Err("⚠️ Usage: `!secret input NAME [description]`".to_string());
    };
    Ok((name.to_string(), parts.collect::<Vec<_>>().join(" ")))
}

fn configure_paste_server_env(command: &mut tokio::process::Command) {
    let env = paste_server_env_from_values(
        std::env::var("CALCIFORGE_PASTE_BIND").ok(),
        std::env::var_os("PASTE_BIND").is_some(),
        std::env::var("CALCIFORGE_PASTE_PUBLIC_BASE_URL").ok(),
        std::env::var("CALCIFORGE_PASTE_PUBLIC_HOST").ok(),
        detect_lan_bind_addr(),
    );

    if let Some(bind) = env.bind {
        command.env("PASTE_BIND", bind);
    }
    if let Some(base_url) = env.public_base_url {
        command.env("PASTE_PUBLIC_BASE_URL", base_url);
    }
    if let Some(host) = env.public_host {
        command.env("PASTE_PUBLIC_HOST", host);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PasteServerEnv {
    bind: Option<String>,
    public_base_url: Option<String>,
    public_host: Option<String>,
}

fn paste_server_env_from_values(
    calciforge_bind: Option<String>,
    inherited_paste_bind_present: bool,
    calciforge_public_base_url: Option<String>,
    calciforge_public_host: Option<String>,
    detected_lan_bind: Option<String>,
) -> PasteServerEnv {
    // Chat-triggered secret input is usually opened from a phone or
    // another LAN machine, so Calciforge binds to the detected LAN address
    // when possible. The standalone paste-server CLI keeps its localhost
    // default, and this path also falls back to that when no LAN address is
    // available.
    let bind = if calciforge_bind.is_some() {
        calciforge_bind
    } else if inherited_paste_bind_present {
        None
    } else {
        detected_lan_bind
    };

    PasteServerEnv {
        bind,
        public_base_url: calciforge_public_base_url,
        public_host: calciforge_public_host,
    }
}

fn detect_lan_bind_addr() -> Option<String> {
    for target in ["192.0.2.1:80", "198.51.100.1:80", "203.0.113.1:80"] {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        if socket.connect(target).is_err() {
            continue;
        }
        let ip = socket.local_addr().ok()?.ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            return Some(format!("{ip}:0"));
        }
    }
    None
}

async fn secure_set(rest: &str) -> String {
    // Accept either `NAME=value` or `NAME value`. The `=` form is
    // natural for env-style keys; the space form is slightly easier
    // on mobile keyboards.
    let (name, value) = match rest.find('=') {
        Some(idx) => {
            let (n, v) = rest.split_at(idx);
            (n.trim().to_string(), v[1..].to_string())
        }
        None => {
            let mut parts = rest.splitn(2, ' ');
            let n = parts.next().unwrap_or("").trim().to_string();
            let v = parts.next().unwrap_or("").to_string();
            (n, v)
        }
    };

    if name.is_empty() || value.is_empty() {
        return "⚠️ Usage: `!secret set NAME=value`".to_string();
    }
    // Keep the accepted syntax narrow so names are safe as fnox keys
    // and as `{{secret:NAME}}` interpolation references in
    // crates/security-proxy/src/substitution.rs.
    if !name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return format!("⚠️ Invalid secret name `{name}` — allowed: A-Z a-z 0-9 _ -");
    }

    // Migrated from a bespoke `Command::new("fnox")` block to the
    // shared FnoxClient — same on-the-wire behavior, but the typed
    // FnoxError lets us distinguish "fnox not installed" from "fnox
    // failed for some other reason" without substring-matching stderr.
    let client = secrets_client::FnoxClient::new();
    match client.set(&name, &value).await {
        Ok(()) => format!(
            "✅ Stored secret `{name}`.\n\n\
             ⚠️ The value you sent is retained by the chat transport. \
             For high-value secrets, use the local paste UI \
             (`paste-server {name}`) or host-local `fnox set {name}`."
        ),
        Err(secrets_client::FnoxError::NotInstalled(e)) => format!(
            "⚠️ fnox not available: {e}. Install it (brew install fnox) \
             and run `fnox init` to enable `!secure`."
        ),
        Err(secrets_client::FnoxError::Failed { stderr, .. }) => {
            // Stderr may name the backend or config file path — that's
            // operational info the user can already read via
            // `fnox doctor`; echoing it here is fine. Crucially does
            // NOT contain the value (we never put it in stderr; fnox
            // certainly doesn't).
            format!("⚠️ fnox set {name} failed: {stderr}")
        }
        Err(other) => format!("⚠️ fnox set {name} failed: {other}"),
    }
}

async fn secure_list() -> String {
    // Migrated to FnoxClient — see secure_set for rationale. The
    // wrapper does the defensive name-extraction parse internally,
    // so we just present the result.
    let client = secrets_client::FnoxClient::new();
    match client.list().await {
        Ok(names) if names.is_empty() => {
            "📭 No secrets stored. Use `paste-server NAME` to add one without chat history."
                .to_string()
        }
        Ok(names) => format!(
            "🔐 {} stored secret{}:\n  {}",
            names.len(),
            if names.len() == 1 { "" } else { "s" },
            names.join("\n  ")
        ),
        Err(secrets_client::FnoxError::NotInstalled(e)) => format!("⚠️ fnox not available: {e}"),
        Err(e) => format!("⚠️ fnox list failed: {e}"),
    }
}

fn active_sessions_message(agent_id: &str, sessions: Vec<String>) -> OutboundMessage {
    let sessions = sessions
        .into_iter()
        .filter(|session| valid_downstream_session_name(session))
        .collect::<Vec<_>>();

    if sessions.is_empty() {
        return OutboundMessage::text(format!(
            "ℹ️ No attachable sessions for '{}'.\n\nUse !switch {} to create a new session.",
            agent_id, agent_id
        ));
    }

    let session_list = sessions.join("\n  - ");
    let reply = format!(
        "🗂️  Active sessions for '{}':\n  - {}\n\nUse !switch {} <session> to attach to a specific session.",
        agent_id, session_list, agent_id
    );
    OutboundMessage::text(reply).with_control(ChoiceControl::new(
        "Attach to a session",
        sessions
            .into_iter()
            .map(|session| ChoiceOption::session(session.clone(), agent_id, session))
            .collect(),
    ))
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
        CalciforgeHeader, CascadeConfig, ChannelAlias, ChannelConfig, DispatcherConfig,
        ExecModelConfig, Identity, RoutingRule, SyntheticModelConfig,
    };
    use crate::providers::alloy::AlloyManager;

    fn make_handler() -> CommandHandler {
        let config = Arc::new(make_config());
        // Use a per-test temp directory so persisted state (`active-agents.json`)
        // never bleeds between test runs.  Without this, a test that calls
        // `handle_switch` writes to the shared active-agent state file
        // file, causing subsequent tests that construct a fresh handler to observe
        // the leftover switch state.
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
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
            &[ExecModelConfig {
                id: "codex/gpt-5.5".to_string(),
                name: Some("Codex GPT-5.5".to_string()),
                context_window: 262_144,
                command: "codex".to_string(),
                args: vec![
                    "exec".to_string(),
                    "-m".to_string(),
                    "gpt-5.5".to_string(),
                    "-".to_string(),
                ],
                env: std::collections::HashMap::new(),
                timeout_seconds: Some(900),
            }],
        )
        .expect("synthetic manager")
    }

    fn make_handler_with_synthetics() -> CommandHandler {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
            .with_alloy_manager(synthetic_manager())
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
                bot_token_file: Some("~/.config/calciforge/secrets/telegram-token".to_string()),
                enabled: true,
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            security: None,
            proxy: None,
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
        assert!(reply.contains("!metrics"));
        assert!(reply.contains("!ping"));
        assert!(reply.contains("!switch"));
        assert!(reply.contains("!agent list"));
        assert!(reply.contains("!secret"));
    }

    // --- !status ---

    #[test]
    fn test_status_handle_returns_none_pre_auth() {
        // !status must NOT be handled pre-auth — it needs identity context
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
        let reply = h.handle_sessions("!session list librarian", "brian").await;
        assert!(
            reply.contains("librarian") && reply.contains("does not support session listing"),
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
    fn test_model_command_lists_all_synthetic_model_classes() {
        let h = make_handler_with_synthetics();
        let reply = h.handle("!model").unwrap();
        assert!(reply.contains("Configured alloys:"), "{reply}");
        assert!(reply.contains("alloy-test"), "{reply}");
        assert!(reply.contains("Configured cascades:"), "{reply}");
        assert!(reply.contains("cascade-test"), "{reply}");
        assert!(reply.contains("Configured dispatchers:"), "{reply}");
        assert!(reply.contains("dispatcher-test"), "{reply}");
        assert!(reply.contains("Configured exec models:"), "{reply}");
        assert!(reply.contains("codex/gpt-5.5"), "{reply}");
    }

    #[test]
    fn test_model_command_activation_becomes_dispatch_override() {
        let h = make_handler_with_synthetics();
        let reply = h.handle_model("!model dispatcher-test", "brian");
        assert!(reply.contains("Activated synthetic model"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn model_noun_aliases_list_and_activate() {
        let h = make_handler_with_synthetics();
        let list = h.handle("!model list").unwrap();
        assert!(list.contains("Configured dispatchers:"), "{list}");
        assert!(list.contains("!model use <synthetic-id>"), "{list}");

        let reply = h.handle_model("!model use dispatcher-test", "brian");
        assert!(reply.contains("Activated synthetic model"), "{reply}");
        assert_eq!(
            h.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[test]
    fn test_model_command_activation_persists_across_handlers() {
        let config = Arc::new(make_config());
        let tmp = tempfile::tempdir().expect("tempdir for test state isolation");
        let state_dir = tmp.path().to_path_buf();

        let h = CommandHandler::with_state_dir(config.clone(), state_dir.clone())
            .with_alloy_manager(synthetic_manager());
        let reply = h.handle_model("!model dispatcher-test", "brian");
        assert!(reply.contains("Activated synthetic model"), "{reply}");

        let restored = CommandHandler::with_state_dir(config, state_dir)
            .with_alloy_manager(synthetic_manager());
        assert_eq!(
            restored.active_model_for_identity("brian").as_deref(),
            Some("dispatcher-test")
        );
    }

    #[tokio::test]
    async fn test_status_shows_active_model_override() {
        let h = make_handler_with_synthetics();
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
        assert!(h
            .cmd_status_for_identity("brian")
            .await
            .contains("version:"));
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

        let model_reply = h.handle_model("!model\tuse\tdispatcher-test", "brian");
        assert!(
            model_reply.contains("Activated synthetic model"),
            "tab-separated model use should succeed: {model_reply}"
        );

        let sessions = h
            .handle_sessions("!session\tlist\tlibrarian", "brian")
            .await;
        assert!(
            sessions.contains("does not support session listing")
                || sessions.contains("No active sessions")
                || sessions.contains("Active sessions"),
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
        let h = make_handler();
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
    fn test_switch_acpx_without_session_clears_prior_session() {
        let h = make_handler();
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
        let h = make_handler();
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

    // -----------------------------------------------------------------------
    // !default command tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_command_not_handled_pre_auth() {
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

    #[test]
    fn session_name_rejects_path_traversal_variants() {
        assert!(!valid_downstream_session_name("../etc/passwd"));
        assert!(!valid_downstream_session_name("foo/bar"));
        assert!(!valid_downstream_session_name("foo\\bar"));
        assert!(!valid_downstream_session_name("a b"));
        assert!(!valid_downstream_session_name("name\0null"));
        assert!(!valid_downstream_session_name(""));
        assert!(!valid_downstream_session_name(&"a".repeat(129)));
        // ".." is technically valid — it's just dots, which are allowed chars.
        // Session names are passed to downstream agents as opaque strings,
        // not used as filesystem paths.
        assert!(valid_downstream_session_name(".."));
    }

    #[test]
    fn session_name_accepts_valid_names() {
        assert!(valid_downstream_session_name("backend"));
        assert!(valid_downstream_session_name("my-session"));
        assert!(valid_downstream_session_name("my_session"));
        assert!(valid_downstream_session_name("v1.2.3"));
        assert!(valid_downstream_session_name("A"));
        assert!(valid_downstream_session_name(&"a".repeat(128)));
    }

    mod session_proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn valid_session_names_contain_no_path_separators(
                name in "[A-Za-z0-9._-]{1,128}"
            ) {
                prop_assert!(valid_downstream_session_name(&name));
                prop_assert!(!name.contains('/'));
                prop_assert!(!name.contains('\\'));
            }

            #[test]
            fn invalid_chars_always_rejected(
                prefix in "[a-z]{0,5}",
                bad_char in prop::sample::select(vec!['/', '\\', ' ', '\t', '\n', '\0', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '=', '+', '{', '}', '[', ']', '|', ';', ':', '\'', '"', '<', '>', ',', '?', '`', '~']),
                suffix in "[a-z]{0,5}",
            ) {
                let name = format!("{prefix}{bad_char}{suffix}");
                prop_assert!(!valid_downstream_session_name(&name),
                    "session name with {:?} should be rejected", bad_char);
            }

            #[test]
            fn overlong_names_rejected(name in "[a-z]{129,200}") {
                prop_assert!(!valid_downstream_session_name(&name));
            }
        }
    }
}
