use crate::adapters::{AgentSessionCapability, agent_session_capability};
use crate::messages::{ChoiceControl, ChoiceOption, OutboundMessage};

use super::{
    CommandHandler, acpx_binary_for_agent, session_runtime_readiness_error,
    state::{save_active_agents_to, save_active_sessions_to},
};

pub(super) fn valid_downstream_session_name(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub(super) fn active_sessions_message(agent_id: &str, sessions: Vec<String>) -> OutboundMessage {
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

impl CommandHandler {
    /// Return the currently selected downstream session for an identity/agent.
    pub fn active_session_for(&self, identity_id: &str, agent_id: &str) -> Option<String> {
        let map = self.active_sessions.lock().unwrap();
        map.get(identity_id)
            .and_then(|sessions| sessions.get(agent_id))
            .cloned()
    }

    fn set_active_session_for(&self, identity_id: &str, agent_id: &str, session: &str) {
        let sessions_snapshot = {
            let mut sessions = self.active_sessions.lock().unwrap();
            sessions
                .entry(identity_id.to_string())
                .or_default()
                .insert(agent_id.to_string(), session.to_string());
            sessions.clone()
        };
        save_active_sessions_to(&self.state_dir, &sessions_snapshot);
    }

    /// Handle a `!switch <agent> [session]` command for an authenticated identity.
    ///
    /// Validates the requested agent against the identity's `allowed_agents`,
    /// updates the active-agent map, and returns a confirmation message.
    /// For session-capable agents, an optional session name can be specified.
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
            return "Usage: !switch <agent> [session]\nAlias: !agent switch <agent> [session]\n\nUse !agent list to see available agents.\nUse !session list <agent> to list available sessions when an adapter can expose them.".to_string();
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

                let session_capability = agent_cfg
                    .map(agent_session_capability)
                    .unwrap_or(AgentSessionCapability::None);
                if session_capability != AgentSessionCapability::None
                    && let Some(session) = session_arg.as_deref()
                    && !valid_downstream_session_name(session)
                {
                    return "⚠️ Invalid session name. Use only letters, numbers, dot, underscore, and dash.".to_string();
                }
                if session_capability != AgentSessionCapability::None
                    && let Some(error) = agent_cfg.and_then(session_runtime_readiness_error)
                {
                    return format!("⚠️ Cannot switch to '{}': {}", agent_id, error);
                }
                let session_info = if session_capability != AgentSessionCapability::None {
                    if let Some(session) = session_arg.as_ref() {
                        format!(" (session: {})", session)
                    } else {
                        " (default session)".to_string()
                    }
                } else if session_arg.is_some() {
                    " (note: session parameter ignored for agents without Calciforge session support)".to_string()
                } else {
                    String::new()
                };

                // Update per-identity active agent and persist to disk.
                let active_agents_snapshot = {
                    let mut map = self.active_agents.lock().unwrap();
                    map.insert(identity_id.to_string(), agent_id.to_string());
                    map.clone()
                };
                save_active_agents_to(&self.state_dir, &active_agents_snapshot);

                let active_sessions_snapshot = {
                    let mut sessions = self.active_sessions.lock().unwrap();
                    if let Some(session) = session_arg
                        .as_ref()
                        .filter(|_| session_capability != AgentSessionCapability::None)
                    {
                        sessions
                            .entry(identity_id.to_string())
                            .or_default()
                            .insert(agent_id.to_string(), session.to_string());
                    } else if session_capability != AgentSessionCapability::None {
                        let mut remove_identity = false;
                        if let Some(identity_sessions) = sessions.get_mut(identity_id) {
                            identity_sessions.remove(agent_id);
                            remove_identity = identity_sessions.is_empty();
                        }
                        if remove_identity {
                            sessions.remove(identity_id);
                        }
                    }
                    sessions.clone()
                };
                save_active_sessions_to(&self.state_dir, &active_sessions_snapshot);

                format!(
                    "✅ Switched to {}{}. Your messages will now route to {}.\n{}",
                    display_name,
                    session_info,
                    agent_id,
                    self.agent_switch_context_notice()
                )
            }
        }
    }

    fn agent_switch_context_notice(&self) -> String {
        if self.config.context.inject_depth == 0 {
            "Context: isolated; no prior thread context will be shared.".to_string()
        } else {
            format!(
                "Context: recent thread context is shared with switched agents (up to {} exchanges).",
                self.config.context.inject_depth
            )
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "kept as a text-only wrapper for callers/tests")
    )]
    pub async fn handle_sessions(&self, text: &str, identity_id: &str) -> String {
        self.handle_sessions_message(text, identity_id)
            .await
            .render_text_fallback()
    }

    /// Start or attach a new named downstream session for the current agent.
    pub fn handle_new_session(&self, text: &str, identity_id: &str) -> String {
        let args: Vec<&str> = text.split_whitespace().skip(1).collect();
        if args.len() > 1 {
            return "Usage: !new [session]\n\nCreates or selects a new named session for your active agent.".to_string();
        }

        let Some(agent_id) = self.active_agent_for(identity_id) else {
            return "⚠️ No active agent is configured for your identity.".to_string();
        };
        let Some(agent_cfg) = self.config.agents.iter().find(|agent| agent.id == agent_id) else {
            return format!(
                "⚠️ Active agent '{}' is not present in configuration.",
                agent_id
            );
        };
        if agent_session_capability(agent_cfg) == AgentSessionCapability::None {
            return format!(
                "ℹ️ Active agent '{}' ({}) does not expose downstream sessions through Calciforge.",
                agent_cfg.id, agent_cfg.kind
            );
        }
        if let Some(error) = session_runtime_readiness_error(agent_cfg) {
            return format!(
                "⚠️ Cannot start a session for '{}': {}",
                agent_cfg.id, error
            );
        }

        let session = args
            .first()
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if !valid_downstream_session_name(&session) {
            return "⚠️ Invalid session name. Use only letters, numbers, dot, underscore, and dash."
                .to_string();
        }

        self.set_active_session_for(identity_id, &agent_id, &session);
        format!(
            "✅ Started session '{}' for {}.\n\nUse !switch {} {} to return to it later.",
            session, agent_id, agent_id, session
        )
    }

    /// Handle a `!sessions` command for an authenticated identity.
    ///
    /// Lists downstream sessions for the specified agent when the adapter supports it.
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
            return OutboundMessage::text(
                "Usage: !sessions <agent>\nAlias: !session list <agent>\n\nLists available downstream sessions when the adapter can expose them.\nUse !agent list to see available agents.",
            );
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

        // Get agent config to check session capability.
        let agent_cfg = match self.config.agents.iter().find(|a| a.id == agent_id) {
            Some(cfg) => cfg,
            None => {
                return OutboundMessage::text(format!(
                    "⚠️ Agent '{}' not found in configuration.",
                    agent_id
                ));
            }
        };

        match agent_session_capability(agent_cfg) {
            AgentSessionCapability::None => OutboundMessage::text(format!(
                "ℹ️ Agent '{}' ({}) does not expose downstream sessions through Calciforge.",
                agent_id, agent_cfg.kind
            )),
            AgentSessionCapability::Named => OutboundMessage::text(format!(
                "ℹ️ Agent '{}' ({}) supports named sessions, but Calciforge cannot list them from the agent yet.\n\nUse !switch {} <session> to attach by name, or !new to create a new Calciforge-managed session for your current agent.",
                agent_id, agent_cfg.kind, agent_id
            )),
            AgentSessionCapability::Listable => {
                let agent_name = agent_cfg.command.as_deref().unwrap_or(agent_id);
                match self.list_acpx_sessions(agent_name, agent_cfg).await {
                    Ok(sessions) if sessions.is_empty() => OutboundMessage::text(format!(
                        "ℹ️ No active sessions for '{}'.\n\nUse !new while '{}' is active to create a new session.",
                        agent_id, agent_id
                    )),
                    Ok(sessions) => active_sessions_message(agent_id, sessions),
                    Err(e) => OutboundMessage::text(format!(
                        "⚠️ Failed to list sessions for '{}': {}\n\nMake sure the session backend is installed and the agent is properly configured.",
                        agent_id, e
                    )),
                }
            }
        }
    }

    /// List ACPX sessions for an agent using the acpx CLI.
    async fn list_acpx_sessions(
        &self,
        agent_name: &str,
        agent_cfg: &crate::config::AgentConfig,
    ) -> Result<Vec<String>, String> {
        tokio::fs::create_dir_all(crate::adapters::acpx::ACPX_SESSION_DIR)
            .await
            .map_err(|e| format!("Failed to create acpx session dir: {}", e))?;

        let acpx_binary = acpx_binary_for_agent(agent_cfg)?;
        let mut command = tokio::process::Command::new(acpx_binary);
        command
            .arg(agent_name)
            .arg("sessions")
            .arg("list")
            .current_dir(crate::adapters::acpx::ACPX_SESSION_DIR);
        if let Some(env) = agent_cfg.env.as_ref() {
            command.envs(env);
        }
        let output = command
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
        let active_agents_snapshot = {
            let mut map = self.active_agents.lock().unwrap();
            map.insert(identity_id.to_string(), default_agent_id.clone());
            map.clone()
        };
        save_active_agents_to(&self.state_dir, &active_agents_snapshot);

        let active_sessions_snapshot = {
            let mut sessions = self.active_sessions.lock().unwrap();
            sessions.remove(identity_id);
            sessions.clone()
        };
        save_active_sessions_to(&self.state_dir, &active_sessions_snapshot);

        format!("✅ Switched to default agent: {}", default_agent_id)
    }
}
