use crate::adapters::agent_supports_model_override;
use crate::messages::{ChoiceControl, ChoiceOption, OutboundMessage};

use super::CommandHandler;
use super::state::save_active_models_to;

impl CommandHandler {
    /// Return the active model override for an identity, if one was selected.
    pub fn active_model_for_identity(&self, identity_id: &str) -> Option<String> {
        if let Some(model) = self.active_models.lock().unwrap().get(identity_id).cloned() {
            return Some(model);
        }
        self.alloy_manager
            .as_ref()
            .and_then(|manager| manager.active_for_identity(identity_id))
    }

    fn set_active_model_for_identity(&self, identity_id: &str, model_id: &str) {
        let mut active_models = self.active_models.lock().unwrap();
        active_models.insert(identity_id.to_string(), model_id.to_string());
        save_active_models_to(&self.state_dir, &active_models);
    }

    /// Return model choices that can be activated with `!model use <id>`.
    pub fn activatable_model_choices(&self) -> Vec<(String, String)> {
        let mut choices = Vec::new();
        choices.extend(
            self.config
                .effective_model_shortcuts()
                .iter()
                .map(|shortcut| {
                    (
                        shortcut.alias.clone(),
                        format!("{} → {}", shortcut.alias, shortcut.model),
                    )
                }),
        );
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
        if let Some(proxy_cfg) = self.config.proxy.as_ref() {
            for provider in &proxy_cfg.providers {
                for model in &provider.models {
                    if model.contains('*') {
                        continue;
                    }
                    choices.push((model.clone(), format!("{} ({})", model, provider.id)));
                }
            }
        }
        choices.sort_by(|left, right| left.0.cmp(&right.0));
        choices.dedup_by(|left, right| left.0 == right.0);
        choices
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

    /// Identity-independent handling for !model — lists shortcuts/alloys.
    /// Returns None if an alloy is being selected (requires post-auth handling).
    pub(super) fn cmd_model_preauth(&self, text: &str) -> Option<String> {
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
            let effective_shortcuts = self.config.effective_model_shortcuts();

            // Model selector aliases include explicit shortcuts plus model roles.
            if !effective_shortcuts.is_empty() {
                lines.push("Model selector aliases:".to_string());
                for shortcut in &effective_shortcuts {
                    lines.push(format!("  {} → {}", shortcut.alias, shortcut.model));
                }
            }

            // Synthetic models section
            if let Some(ref manager) = self.alloy_manager
                && !manager.is_empty()
            {
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
            }

            if lines.is_empty() {
                return Some("No model shortcuts or gateway model selectors configured.\n\nAdd shortcuts to your config:\n[[model_shortcuts]]\nalias = \"sonnet\"\nmodel = \"anthropic/claude-sonnet-4.6\"".to_string());
            }

            lines.push("\nUsage:".to_string());
            lines.push("  !model list — show this list".to_string());
            lines.push("  !model <alias> — activate the shortcut target".to_string());
            if self.alloy_manager.is_some() {
                lines.push(
                    "  !model use <id> — activate an alloy/cascade/dispatcher for your identity"
                        .to_string(),
                );
            }
            Some(lines.join("\n"))
        } else {
            // Argument provided — defer to post-auth handling for activatable
            // shortcuts, synthetic routing selectors, local models, and provider models.
            if args.first().is_some_and(|arg| {
                arg.eq_ignore_ascii_case("use")
                    || arg.eq_ignore_ascii_case("switch")
                    || arg.eq_ignore_ascii_case("set")
            }) {
                args.remove(0);
            }

            // Return None to trigger post-auth handling for activatable model
            // selections. Provider-backed and local model choices do not
            // require an alloy manager.
            None
        }
    }

    /// Handle a `!model <id>` command for an authenticated identity.
    ///
    /// Dispatch order:
    /// 1. If the ID matches a gateway model selector → activate it.
    /// 2. If the ID matches a local model in `[local_models]` → trigger a switch
    ///    (async background task, returns immediately with status message).
    /// 3. If the ID matches a `[[proxy.providers]]` concrete model → activate it.
    ///    Provider `on_switch` hooks run synchronously at gateway request time.
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

        let requested_model_id = args[0];
        let effective_shortcuts = self.config.effective_model_shortcuts();
        let resolved_model_id = match crate::model_names::resolve_model_alias_chain(
            &effective_shortcuts,
            requested_model_id,
        ) {
            Ok(model_id) => model_id,
            Err(e) => return format!("⚠️ {e}"),
        };
        let model_id = resolved_model_id.as_str();
        let shortcut_note = if model_id == requested_model_id {
            None
        } else {
            Some(format!(" via alias '{requested_model_id}' → '{model_id}'"))
        };

        let Some(active_agent_id) = self.active_agent_for(identity_id) else {
            return "⚠️ No active agent is configured for your identity, so Calciforge cannot apply a model override.".to_string();
        };
        let Some(active_agent) = self
            .config
            .agents
            .iter()
            .find(|agent| agent.id == active_agent_id)
        else {
            return format!(
                "⚠️ Active agent '{}' is not present in configuration.",
                active_agent_id
            );
        };
        if !agent_supports_model_override(active_agent) {
            return format!(
                "⚠️ Active agent '{}' ({}) does not consume Calciforge model overrides.\n\nUse an agent explicitly configured with allow_model_override = true, or configure this agent's native model setting instead. Only enable that flag for agents wired to Calciforge's model gateway or known to accept these model IDs.",
                active_agent.id, active_agent.kind
            );
        }

        // 1. Synthetic model selector switch.
        if let Some(ref manager) = self.alloy_manager
            && manager.is_synthetic_model(model_id)
        {
            if let Err(e) = manager.set_active_for_identity(identity_id, model_id) {
                return format!("⚠️ Failed to activate model: {}", e);
            }
            self.set_active_model_for_identity(identity_id, model_id);
            if let Some(alloy) = manager.get(model_id) {
                let constituents: Vec<String> = alloy
                    .definition()
                    .constituents
                    .iter()
                    .map(|c| format!("{} (weight {})", c.model, c.weight))
                    .collect();
                return format!(
                    "✅ Activated alloy '{}'{} for your identity.\n\nConstituents ({:?} strategy): {}",
                    model_id,
                    shortcut_note.as_deref().unwrap_or(""),
                    alloy.definition().strategy,
                    constituents.join(", ")
                );
            }
            let kind = if manager
                .list_cascades()
                .iter()
                .any(|cascade| cascade.id == model_id)
            {
                "cascade"
            } else if manager
                .list_dispatchers()
                .iter()
                .any(|dispatcher| dispatcher.id == model_id)
            {
                "dispatcher"
            } else {
                "synthetic model selector"
            };
            return format!(
                "✅ Activated {kind} '{}'{} for your identity.",
                model_id,
                shortcut_note.as_deref().unwrap_or("")
            );
        }

        // 2. Local model switch.
        if let Some(ref lm_mgr) = self.local_manager
            && let Some(model_def) = lm_mgr.find_model(model_id)
        {
            let hf_id = model_def.hf_id.clone();
            let id = model_id.to_string();
            let mgr = crate::sync::Arc::clone(lm_mgr);
            self.set_active_model_for_identity(identity_id, model_id);
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
                "🔄 Switching to local model '{}'{} (HF: {}).\n\
                    This may take 1-2 minutes while the model loads.\n\
                    The gateway will continue serving requests during the transition.",
                model_id,
                shortcut_note.as_deref().unwrap_or(""),
                hf_id
            );
        }

        // 3. Provider-backed concrete model.
        if let Some(ref proxy_cfg) = self.config.proxy {
            for provider in &proxy_cfg.providers {
                let model_matches = provider
                    .models
                    .iter()
                    .any(|p| crate::proxy::routing::model_matches_pattern(model_id, p));
                if model_matches {
                    self.set_active_model_for_identity(identity_id, model_id);
                    if let Some(ref hook_script) = provider.on_switch
                        && !hook_script.is_empty()
                    {
                        return format!(
                            "✅ Activated model '{}'{} for your identity (provider: {}). Its on_switch hook will run before the next gateway request that uses this provider.",
                            model_id,
                            shortcut_note.as_deref().unwrap_or(""),
                            provider.id
                        );
                    }
                    return format!(
                        "✅ Activated model '{}'{} for your identity (provider: {}).",
                        model_id,
                        shortcut_note.as_deref().unwrap_or(""),
                        provider.id
                    );
                }
            }
        }

        // 4. Unknown model — show what's available.
        let mut available = vec![];
        for shortcut in &effective_shortcuts {
            available.push(format!("  {} → {} (alias)", shortcut.alias, shortcut.model));
        }
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
        }
        if let Some(ref lm) = self.local_manager {
            for m in lm.models() {
                available.push(format!("  {} (local/{})", m.id, m.provider_type));
            }
        }
        if available.is_empty() {
            return format!(
                "⚠️ Unknown model: '{}'\n\nNo models configured.",
                requested_model_id
            );
        }
        format!(
            "⚠️ Unknown model: '{}'\n\nAvailable:\n{}",
            requested_model_id,
            available.join("\n")
        )
    }
}
