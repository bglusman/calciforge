//! Configuration validator — catches config errors before runtime.
//!
//! Validates:
//! - No duplicate IDs (agents, identities, channels, alloys, etc.)
//! - All referenced agents exist in routing rules
//! - Valid port numbers and URLs
//! - TOML syntax is well-formed
//! - Required fields are present
//! - No circular dependencies

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use url::Url;

use crate::agent_kinds::{AgentKind, parse_agent_kind};
use crate::config::{CalciforgeConfig, CredentialOwner, GatewayRetryConfig, MatrixE2eeMode};
use crate::model_names::{
    configured_agent_selectors, configured_first_class_model_ids, resolve_model_alias_chain,
};

/// Validation result with detailed error messages.
#[derive(Debug)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn new() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn add_error(&mut self, msg: String) {
        self.valid = false;
        self.errors.push(msg);
    }

    pub fn add_warning(&mut self, msg: String) {
        self.warnings.push(msg);
    }

    pub fn is_valid(&self) -> bool {
        self.valid && self.errors.is_empty()
    }
}

/// Validate a complete CalciforgeConfig.
pub fn validate_config(config: &CalciforgeConfig) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Check for duplicate IDs
    validate_no_duplicate_ids(config, &mut result);

    // Validate adapter kinds and required per-kind fields
    validate_agents(config, &mut result);

    // Validate routing rules reference valid agents
    validate_routing_rules(config, &mut result);

    // Validate identities have valid channels
    validate_identities(config, &mut result);

    // Validate enabled channels before long-lived tasks start.
    validate_channels(config, &mut result);

    // Validate alloys have valid constituents
    validate_alloys(config, &mut result);

    // Validate cascades and dispatchers
    validate_synthetic_model_groups(config, &mut result);

    // Validate proxy configuration if present
    if let Some(ref proxy) = config.proxy {
        validate_proxy_config(proxy, &mut result);
    }

    // Validate security settings
    if let Some(ref security) = config.security {
        validate_security_config(security, &mut result);
    }

    result
}

/// Validate agent adapter kinds and required fields.
fn validate_agents(config: &CalciforgeConfig, result: &mut ValidationResult) {
    for agent in &config.agents {
        match parse_agent_kind(&agent.kind) {
            Some(AgentKind::OpenClawChannel) => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' uses openclaw-channel but has no endpoint",
                        agent.id
                    ));
                }
                if agent.api_key.is_none()
                    && agent.api_key_file.is_none()
                    && agent.auth_token.is_none()
                {
                    result.add_warning(format!(
                        "Agent '{}' uses openclaw-channel without api_key/api_key_file/auth_token; no per-agent token is configured, though adapters may still fall back to CALCIFORGE_AGENT_TOKEN. Only loopback gateways intended to rely on that setup should do this",
                        agent.id
                    ));
                }
                if agent.reply_auth_token.is_none() && agent.reply_auth_token_file.is_none() {
                    result.add_warning(format!(
                        "Agent '{}' uses openclaw-channel without reply_auth_token/reply_auth_token_file; callback replies should be bearer-protected outside isolated local tests",
                        agent.id
                    ));
                }
            }
            Some(AgentKind::OpenAiCompat) => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' uses openai-compat but has no endpoint",
                        agent.id
                    ));
                }
                if agent.model.as_deref().is_some_and(is_openclaw_model_id) {
                    result.add_error(format!(
                        "Agent '{}' uses openai-compat with OpenClaw model '{}'; OpenClaw agent chat must use kind='openclaw-channel'",
                        agent.id,
                        agent.model.as_deref().unwrap_or_default()
                    ));
                }
                if agent.model.is_none() && agent.allow_model_override != Some(true) {
                    result.add_error(format!(
                        "Agent '{}' uses openai-compat without a configured model; set model or allow_model_override = true to forward !model overrides",
                        agent.id
                    ));
                }
                if agent.api_key.is_none()
                    && agent.api_key_file.is_none()
                    && agent.auth_token.is_none()
                {
                    result.add_warning(format!(
                        "Agent '{}' uses openai-compat without api_key/api_key_file/auth_token; only unauthenticated local endpoints should do this",
                        agent.id
                    ));
                }
            }
            Some(AgentKind::ZeroClaw) => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires endpoint",
                        agent.id, agent.kind
                    ));
                }
                if agent.api_key.is_none() && agent.api_key_file.is_none() {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires api_key or api_key_file",
                        agent.id, agent.kind
                    ));
                }
            }
            Some(AgentKind::IronClaw | AgentKind::Hermes) => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires endpoint",
                        agent.id, agent.kind
                    ));
                }
                if agent.api_key.is_none()
                    && agent.api_key_file.is_none()
                    && agent.auth_token.is_none()
                {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires api_key, api_key_file, or auth_token",
                        agent.id, agent.kind
                    ));
                }
            }
            Some(AgentKind::ZeroClawHttp | AgentKind::ZeroClawNative) => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires endpoint",
                        agent.id, agent.kind
                    ));
                }
            }
            Some(
                AgentKind::Exec
                | AgentKind::Cli
                | AgentKind::ArtifactCli
                | AgentKind::Acp
                | AgentKind::Acpx,
            ) => {
                if agent
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty())
                {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires command",
                        agent.id, agent.kind
                    ));
                }
            }
            Some(
                AgentKind::CodexCli
                | AgentKind::ClaudeCli
                | AgentKind::DiracCli
                | AgentKind::KimiCli,
            ) => {}
            None if agent.kind == "openclaw-http" => {
                result.add_error(format!(
                    "Agent '{}' uses removed kind 'openclaw-http'; migrate to kind='openclaw-channel' and install the Calciforge OpenClaw channel plugin",
                    agent.id
                ));
            }
            None if agent.kind == "openclaw-native" => {
                result.add_error(format!(
                    "Agent '{}' uses unsupported kind 'openclaw-native'; /hooks/agent is async automation, not a synchronous chat adapter. Use kind='openclaw-channel'",
                    agent.id
                ));
            }
            None => {
                result.add_error(format!(
                    "Agent '{}' has unknown kind '{}'",
                    agent.id, agent.kind
                ));
            }
        }
    }
}

fn is_openclaw_model_id(model: &str) -> bool {
    let trimmed = model.trim();
    trimmed == "openclaw" || trimmed.starts_with("openclaw/")
}

fn validate_channels(config: &CalciforgeConfig, result: &mut ValidationResult) {
    for channel in &config.channels {
        match channel.kind.as_str() {
            "whatsapp" => {
                validate_no_legacy_embedded_channel_fields("WhatsApp", channel, result);
                if channel.enabled && channel.whatsapp_session_path.is_none() {
                    result.add_error(
                        "WhatsApp channel requires whatsapp_session_path when enabled".to_string(),
                    );
                }
            }
            "signal" => {
                validate_no_legacy_embedded_channel_fields("Signal", channel, result);
                if channel.enabled && channel.signal_cli_url.is_none() {
                    result.add_error(
                        "Signal channel requires signal_cli_url when enabled".to_string(),
                    );
                }
                if channel.enabled && channel.signal_account.is_none() {
                    result.add_error(
                        "Signal channel requires signal_account when enabled".to_string(),
                    );
                }
            }
            "matrix" => {
                if channel.enabled && channel.homeserver.is_none() {
                    result.add_error("Matrix channel requires homeserver when enabled".to_string());
                }
                if channel.enabled && channel.access_token_file.is_none() {
                    result.add_error(
                        "Matrix channel requires access_token_file when enabled".to_string(),
                    );
                }
                if channel.enabled && channel.allowed_users.is_empty() {
                    result.add_error(
                        "Matrix channel requires at least one allowed_user when enabled"
                            .to_string(),
                    );
                }
                match channel.matrix_e2ee {
                    MatrixE2eeMode::Off | MatrixE2eeMode::Warn => {
                        if channel.matrix_e2ee_store_path.is_some() {
                            result.add_warning(
                                "Matrix channel sets matrix_e2ee_store_path, but matrix_e2ee is not experimental-sdk; the store path will be ignored"
                                    .to_string(),
                            );
                        }
                    }
                    MatrixE2eeMode::Require => {
                        if channel.enabled && channel.room_id.is_none() {
                            result.add_error(
                                "Matrix channel matrix_e2ee='require' needs room_id so Calciforge can fail closed on encrypted-room state"
                                    .to_string(),
                            );
                        }
                    }
                    MatrixE2eeMode::ExperimentalSdk => {
                        if channel.enabled && channel.room_id.is_none() {
                            result.add_error(
                                "Matrix channel matrix_e2ee='experimental-sdk' needs room_id for the prototype; joined-room autodiscovery is not implemented"
                                    .to_string(),
                            );
                        }
                        if channel.enabled && channel.matrix_e2ee_store_path.is_none() {
                            result.add_error(
                                "Matrix channel matrix_e2ee='experimental-sdk' requires matrix_e2ee_store_path for persistent crypto state"
                                    .to_string(),
                            );
                        }
                        if channel.enabled && !cfg!(feature = "channel-matrix-e2ee") {
                            result.add_error(
                                "Matrix channel matrix_e2ee='experimental-sdk' requires a calciforge build with --features channel-matrix-e2ee"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn validate_no_legacy_embedded_channel_fields(
    channel_name: &str,
    channel: &crate::config::ChannelConfig,
    result: &mut ValidationResult,
) {
    let legacy_fields = [
        ("zeroclaw_endpoint", channel.zeroclaw_endpoint.as_ref()),
        ("zeroclaw_auth_token", channel.zeroclaw_auth_token.as_ref()),
        ("webhook_listen", channel.webhook_listen.as_ref()),
        ("webhook_path", channel.webhook_path.as_ref()),
        ("webhook_secret", channel.webhook_secret.as_ref()),
    ];

    for (field, value) in legacy_fields {
        if value.is_some() {
            let msg = format!(
                "{channel_name} channel uses legacy field '{field}'. Embedded {channel_name} no longer supports ZeroClaw/OpenClaw webhook sidecars; remove the legacy field and use the embedded channel schema."
            );
            if channel.enabled {
                result.add_error(msg);
            } else {
                result.add_warning(msg);
            }
        }
    }
}

/// Check for duplicate IDs across all config sections.
fn validate_no_duplicate_ids(config: &CalciforgeConfig, result: &mut ValidationResult) {
    // Check duplicate identity IDs
    let mut identity_ids = HashSet::new();
    for identity in &config.identities {
        if !identity_ids.insert(&identity.id) {
            result.add_error(format!("Duplicate identity ID: '{}'", identity.id));
        }
    }

    // Check duplicate agent IDs
    let mut agent_ids = HashSet::new();
    for agent in &config.agents {
        if !agent_ids.insert(&agent.id) {
            result.add_error(format!("Duplicate agent ID: '{}'", agent.id));
        }
    }

    // Check duplicate channel kinds (basic check)
    let mut channel_kinds = HashSet::new();
    for channel in &config.channels {
        if !channel_kinds.insert(&channel.kind) {
            result.add_warning(format!(
                "Multiple configurations for channel kind: '{}'",
                channel.kind
            ));
        }
    }

    let configured_model_ids = configured_first_class_model_ids(config);
    let configured_agent_selectors = configured_agent_selectors(config);

    // Check duplicate agent selectors that would make chat routing ambiguous.
    let mut agent_selectors = HashMap::new();
    for selector in &configured_agent_selectors {
        if let Some((previous_owner, previous_kind)) = agent_selectors.insert(
            selector.id.as_str(),
            (&selector.owner_agent_id, selector.kind),
        ) && previous_owner != &selector.owner_agent_id
        {
            result.add_error(format!(
                    "Ambiguous agent selector '{}': configured as a {} for agent '{}' and a {} for agent '{}'. Agent IDs and aliases must resolve to one chat target.",
                    selector.id,
                    previous_kind.label(),
                    previous_owner,
                    selector.kind.label(),
                    selector.owner_agent_id
                ));
        }
    }

    // Check duplicate configured first-class model IDs across all configured
    // model namespaces that are visible to callers.
    let mut model_ids = HashMap::new();
    for entry in &configured_model_ids {
        if let Some(previous_kind) = model_ids.insert(entry.id.as_str(), entry.kind) {
            result.add_error(format!(
                "Ambiguous model selector '{}': configured as both a {} and a {}. Model selectors must be unique across synthetic routing selectors, local models, exact provider models, and exact model routes.",
                entry.id,
                previous_kind.label(),
                entry.kind.label()
            ));
        }
        if let Some(conflict) = configured_agent_selectors
            .iter()
            .find(|selector| selector.id == entry.id)
        {
            result.add_error(format!(
                "Ambiguous selector '{}': configured as a {} and a {}. Agent selectors are used for routing/chat targets; model selectors are used for gateway model requests. Rename one side so Calciforge cannot treat an agent name as a model name.",
                entry.id,
                entry.kind.label(),
                conflict.kind.label()
            ));
        }
    }

    let effective_shortcuts = config.effective_model_shortcuts();

    // Check duplicate model shortcut aliases and model roles. Roles share the
    // public model-selector namespace with shortcuts by design.
    let mut shortcut_aliases = HashSet::new();
    for shortcut in &effective_shortcuts {
        if !shortcut_aliases.insert(&shortcut.alias) {
            result.add_error(format!(
                "Duplicate model shortcut alias or role: '{}'",
                shortcut.alias
            ));
        }
        if let Some(conflict) = configured_model_ids
            .iter()
            .find(|entry| entry.id == shortcut.alias)
        {
            result.add_error(format!(
                "Ambiguous model shortcut alias '{}': conflicts with a configured {}. Shortcuts must point to configured models; they cannot reuse a first-class model selector.",
                shortcut.alias,
                conflict.kind.label()
            ));
        }
        if let Some(conflict) = configured_agent_selectors
            .iter()
            .find(|selector| selector.id == shortcut.alias)
        {
            result.add_error(format!(
                "Ambiguous model shortcut alias '{}': conflicts with a configured {} for agent '{}'. Agent selectors route chat targets; model shortcuts select gateway models. Rename one side.",
                shortcut.alias,
                conflict.kind.label(),
                conflict.owner_agent_id
            ));
        }
    }
    for shortcut in &effective_shortcuts {
        if let Err(e) = resolve_model_alias_chain(&effective_shortcuts, &shortcut.alias) {
            result.add_error(e);
        }
    }
}

/// Validate routing rules reference valid agents.
fn validate_routing_rules(config: &CalciforgeConfig, result: &mut ValidationResult) {
    let valid_agents: HashSet<_> = config.agents.iter().map(|a| &a.id).collect();

    for rule in &config.routing {
        // Check default_agent exists
        if !valid_agents.contains(&rule.default_agent) {
            result.add_error(format!(
                "Routing rule for '{}' references non-existent agent: '{}'",
                rule.identity, rule.default_agent
            ));
        }

        if let Some(btw_agent) = rule.btw_agent.as_deref() {
            if !config.agents.iter().any(|agent| agent.id == btw_agent) {
                result.add_error(format!(
                    "Routing rule for '{}' references non-existent btw_agent: '{}'",
                    rule.identity, btw_agent
                ));
            }
            if !rule.allowed_agents.is_empty()
                && !rule.allowed_agents.iter().any(|agent| agent == btw_agent)
            {
                result.add_error(format!(
                    "Routing rule for '{}' sets btw_agent '{}' outside allowed_agents",
                    rule.identity, btw_agent
                ));
            }
        }

        // Check all allowed_agents exist
        for agent_id in &rule.allowed_agents {
            if !valid_agents.contains(agent_id) {
                result.add_error(format!(
                    "Routing rule for '{}' allows non-existent agent: '{}'",
                    rule.identity, agent_id
                ));
            }
        }
    }
}

/// Validate identities have valid channel aliases.
fn validate_identities(config: &CalciforgeConfig, result: &mut ValidationResult) {
    let valid_channels: HashSet<_> = config.channels.iter().map(|c| c.kind.clone()).collect();

    for identity in &config.identities {
        for alias in &identity.aliases {
            if !valid_channels.contains(&alias.channel) {
                result.add_warning(format!(
                    "Identity '{}' has alias for unconfigured channel: {:?}",
                    identity.id, alias.channel
                ));
            }
        }
    }
}

/// Validate alloy configurations.
fn validate_alloys(config: &CalciforgeConfig, result: &mut ValidationResult) {
    for alloy in &config.alloys {
        // Check strategy is valid
        match alloy.strategy.as_str() {
            "weighted" | "round_robin" => {}
            other => {
                result.add_error(format!(
                    "Alloy '{}' has invalid strategy: '{}'. Use 'weighted' or 'round_robin'",
                    alloy.id, other
                ));
            }
        }

        // Check constituents sum to reasonable weight for weighted strategy
        if alloy.strategy == "weighted" && !alloy.constituents.is_empty() {
            let total_weight: u32 = alloy.constituents.iter().map(|c| c.weight).sum();
            if total_weight == 0 {
                result.add_error(format!(
                    "Alloy '{}' has constituents with zero total weight",
                    alloy.id
                ));
            }
        }

        // Warn if alloy has no constituents
        if alloy.constituents.is_empty() {
            result.add_warning(format!(
                "Alloy '{}' has no constituents and will be unusable",
                alloy.id
            ));
        }
    }
}

/// Validate named synthetic model selectors.
fn validate_synthetic_model_groups(config: &CalciforgeConfig, result: &mut ValidationResult) {
    for cascade in &config.cascades {
        if cascade.models.is_empty() {
            result.add_error(format!("Cascade '{}' has no models", cascade.id));
        }
        for model in &cascade.models {
            if model.model.trim().is_empty() {
                result.add_error(format!("Cascade '{}' has an empty model id", cascade.id));
            }
            if model.context_window == 0 {
                result.add_error(format!(
                    "Cascade '{}' model '{}' has context_window=0",
                    cascade.id, model.model
                ));
            }
        }
    }

    for dispatcher in &config.dispatchers {
        if dispatcher.models.is_empty() {
            result.add_error(format!("Dispatcher '{}' has no models", dispatcher.id));
        }
        for model in &dispatcher.models {
            if model.model.trim().is_empty() {
                result.add_error(format!(
                    "Dispatcher '{}' has an empty model id",
                    dispatcher.id
                ));
            }
            if model.context_window == 0 {
                result.add_error(format!(
                    "Dispatcher '{}' model '{}' has context_window=0",
                    dispatcher.id, model.model
                ));
            }
        }
    }

    if !config.exec_models.is_empty() {
        result.add_error(
            "`[[exec_models]]` is deprecated and no longer registers gateway models; configure an `[[agents]]` entry with kind = \"exec\", \"codex-cli\", \"dirac-cli\", or an ACP adapter instead."
                .to_string(),
        );
    }
}

/// Validate proxy configuration.
fn validate_proxy_config(proxy: &crate::config::ProxyConfig, result: &mut ValidationResult) {
    if let Some(url) = proxy.gateway_ui_url.as_deref() {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            result.add_error("Proxy gateway_ui_url cannot be blank when set".to_string());
        } else {
            validate_http_url("Proxy gateway_ui_url", trimmed, result, true);
        }
    }

    if !proxy.enabled {
        return;
    }

    validate_gateway_retry_config("Proxy retry", &proxy.retry, result);

    // Validate bind address format
    if let Err(e) = proxy.bind.parse::<std::net::SocketAddr>() {
        result.add_error(format!(
            "Proxy bind address '{}' is invalid: {}",
            proxy.bind, e
        ));
    }

    // Validate timeout is reasonable
    if proxy.timeout_seconds == 0 {
        result.add_error("Proxy timeout_seconds cannot be zero".to_string());
    } else if proxy.timeout_seconds > 3600 {
        result.add_warning(format!(
            "Proxy timeout_seconds ({}) is very high (> 1 hour)",
            proxy.timeout_seconds
        ));
    }

    if proxy.providers.is_empty() && proxy.backend_type == "mock" {
        result.add_error(
            "Proxy enabled with no [[proxy.providers]] uses the mock provider adapter. Configure one or more explicit provider adapters, or set an explicit non-mock root backend_type for compatibility."
                .to_string(),
        );
    }

    let root_gateway_type = proxy
        .backend_type
        .parse::<crate::proxy::gateway::GatewayType>()
        .ok();

    if root_gateway_type.is_some_and(|gateway_type| gateway_type.requires_backend_url())
        && proxy.backend_url.trim().is_empty()
    {
        result.add_error(format!(
            "Proxy enabled with root backend_type='{}' requires backend_url. Use backend_type='mock' for an explicit-provider-only config where unmatched models should fail instead of falling back to a root provider.",
            proxy.backend_type
        ));
    }

    // Validate backend_type against the same allowlist the runtime uses.
    if !crate::proxy::supported_root_gateway_backend_types()
        .iter()
        .any(|backend_type| *backend_type == proxy.backend_type)
    {
        result.add_error(format!(
            "Proxy backend_type '{}' is unsupported. Use one of: {}. CLI-backed agents and experimental external gateways must be configured as agents or explicit provider adapters.",
            proxy.backend_type,
            crate::proxy::supported_root_gateway_backend_types().join(", ")
        ));
    }

    if proxy.backend_type == "http" {
        result.add_warning(
            "Proxy backend_type='http' uses Calciforge's legacy root builtin HTTP adapter. Prefer explicit [[proxy.providers]] adapters so model routing, aliases, credentials, and audit boundaries are scoped per provider."
                .to_string(),
        );
    }
    if proxy.backend_type == "mock" {
        result.add_warning(
            "Proxy backend_type='mock' returns deterministic local test responses and is test-only. Operational installs must choose explicit provider adapters."
                .to_string(),
        );
    }

    if root_gateway_type.is_some_and(|gateway_type| gateway_type.requires_backend_url()) {
        let backend_url = proxy.backend_url.trim();
        if backend_url.is_empty() {
            result.add_error(format!(
                "Proxy backend_url cannot be blank for backend_type='{}'",
                proxy.backend_type
            ));
        } else {
            validate_http_url("Proxy backend_url", backend_url, result, false);
        }
    }

    if proxy.backend_type == "helicone" {
        let has_inline_key = proxy
            .backend_api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|key| !key.is_empty());
        if !has_inline_key && proxy.backend_api_key_file.is_none() {
            result.add_warning(
                "Helicone backend has no backend_api_key/backend_api_key_file; only unauthenticated local Helicone gateways should use this"
                    .to_string(),
            );
        }
    }

    for provider in &proxy.providers {
        let provider_gateway_type = provider
            .backend_type
            .parse::<crate::proxy::gateway::GatewayType>()
            .ok();
        if provider.backend_type == "exec" {
            result.add_error(format!(
                "Proxy provider '{}' uses deprecated backend_type = \"exec\". CLI-backed subscriptions must be configured as [[agents]], not gateway providers.",
                provider.id
            ));
        } else if !provider_gateway_type
            .is_some_and(|gateway_type| gateway_type.uses_openai_compatible_http_core())
        {
            result.add_error(format!(
                "Proxy provider '{}' backend_type '{}' is invalid. Use one of: {}",
                provider.id,
                provider.backend_type,
                crate::proxy::gateway::GatewayType::SUPPORTED_PROVIDER_CONFIG_NAMES.join(", ")
            ));
        }

        if provider_gateway_type
            .is_some_and(|gateway_type| gateway_type.uses_openai_compatible_http_core())
            && provider.url.trim().is_empty()
        {
            result.add_error(format!(
                "Proxy provider '{}' with backend_type='{}' requires url",
                provider.id, provider.backend_type
            ));
        }

        if provider.backend_type == "http" {
            match provider.model_credential_owner {
                CredentialOwner::Provider => result.add_warning(format!(
                    "Proxy provider '{}' uses backend_type='http' with model_credential_owner='provider'; treating the endpoint as a provider-owned OpenAI-compatible boundary such as LiteLLM. Upstream model/provider keys are expected to live in that provider boundary, not Calciforge.",
                    provider.id
                )),
                CredentialOwner::Calciforge => result.add_warning(format!(
                    "Proxy provider '{}' uses Calciforge's builtin HTTP upstream adapter. This is a minimal compatibility path, not a provider-owned boundary; Helicone/LiteLLM observability, provider registry, and provider-owned retry/key behavior will not apply to this route.",
                    provider.id
                )),
            }
        }

        if provider
            .strip_model_prefix
            .as_deref()
            .map(str::trim)
            .is_some_and(str::is_empty)
        {
            result.add_error(format!(
                "Proxy provider '{}' strip_model_prefix cannot be empty",
                provider.id
            ));
        }

        if let Some(prefix) = provider
            .strip_model_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
        {
            let has_prefixed_model = provider.models.iter().any(|model| {
                model == "*"
                    || model.starts_with(prefix)
                    || model
                        .strip_suffix("/*")
                        .is_some_and(|model_prefix| prefix.starts_with(model_prefix))
            });
            if !has_prefixed_model {
                result.add_warning(format!(
                    "Proxy provider '{}' strips model prefix '{}' but none of its models use that prefix",
                    provider.id, prefix
                ));
            }
        }

        if let Some(retry) = provider.retry.as_ref() {
            validate_gateway_retry_config(
                &format!("Proxy provider '{}' retry", provider.id),
                retry,
                result,
            );
        }

        let has_provider_auth = provider
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|key| !key.is_empty())
            || provider.api_key_file.is_some();
        let has_model_auth = provider
            .model_api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|key| !key.is_empty())
            || provider.model_api_key_file.is_some();

        match provider.model_credential_owner {
            CredentialOwner::Calciforge => {
                if !has_model_auth && !has_provider_auth {
                    result.add_error(format!(
                        "Proxy provider '{}' has model_credential_owner='calciforge' but no model_api_key/model_api_key_file or legacy api_key/api_key_file. Calciforge-owned model credentials must be explicit; use model_credential_owner='provider' when the upstream provider boundary owns model credentials or no final model credential is required.",
                        provider.id
                    ));
                } else if has_model_auth && has_provider_auth {
                    result.add_error(format!(
                        "Proxy provider '{}' configures both provider adapter auth (api_key/api_key_file) and Calciforge-owned model auth (model_api_key/model_api_key_file). The current builtin provider adapters send one bearer credential per request; use model_api_key/model_api_key_file for direct upstream model auth, or model_credential_owner='provider' when the provider boundary owns final model credentials.",
                        provider.id
                    ));
                } else if !has_model_auth && has_provider_auth {
                    result.add_warning(format!(
                        "Proxy provider '{}' uses legacy api_key/api_key_file as both provider endpoint auth and Calciforge-owned model auth; prefer model_api_key/model_api_key_file when those credentials are conceptually separate",
                        provider.id
                    ));
                }
            }
            CredentialOwner::Provider => {
                if has_provider_auth {
                    result.add_warning(format!(
                        "Proxy provider '{}' has model_credential_owner='provider'; api_key/api_key_file authenticate Calciforge to the provider boundary endpoint, not to the upstream model provider",
                        provider.id
                    ));
                }
                if has_model_auth {
                    result.add_error(format!(
                        "Proxy provider '{}' has model_credential_owner='provider' but also configures model_api_key/model_api_key_file; final model credentials are owned by the provider boundary, not Calciforge",
                        provider.id
                    ));
                }
            }
        }
    }
}

fn validate_gateway_retry_config(
    label: &str,
    retry: &GatewayRetryConfig,
    result: &mut ValidationResult,
) {
    if retry.min_timeout_ms == 0 {
        result.add_error(format!("{label} min_timeout_ms cannot be zero"));
    }
    if retry.max_timeout_ms == 0 {
        result.add_error(format!("{label} max_timeout_ms cannot be zero"));
    }
    if retry.min_timeout_ms > retry.max_timeout_ms {
        result.add_error(format!(
            "{label} min_timeout_ms ({}) cannot exceed max_timeout_ms ({})",
            retry.min_timeout_ms, retry.max_timeout_ms
        ));
    }
    if retry.factor == 0 {
        result.add_error(format!("{label} factor cannot be zero"));
    }
    if retry.enabled && retry.max_retries == 0 {
        result.add_warning(format!(
            "{label} is enabled but max_retries=0; requests will not actually be retried"
        ));
    }
    if retry.enabled && retry.retry_on.is_empty() {
        result.add_warning(format!(
            "{label} is enabled but retry_on is empty; requests will not actually be retried"
        ));
    }
}

fn validate_http_url(
    field: &str,
    value: &str,
    result: &mut ValidationResult,
    allow_query_or_fragment: bool,
) {
    match Url::parse(value) {
        Ok(url) => {
            if !matches!(url.scheme(), "http" | "https") {
                result.add_error(format!("{field} '{}' must use http:// or https://", value));
            }
            if !allow_query_or_fragment && (url.query().is_some() || url.fragment().is_some()) {
                result.add_error(format!(
                    "{field} '{}' must not include query parameters or fragments",
                    value
                ));
            }
        }
        Err(e) => {
            result.add_error(format!("{field} '{}' is invalid: {}", value, e));
        }
    }
}

/// Validate security configuration.
fn validate_security_config(
    security: &crate::config::SecuritySectionConfig,
    result: &mut ValidationResult,
) {
    // Validate adversary detector profile
    match security.profile.as_str() {
        "open" | "balanced" | "hardened" | "maximum" | "paranoid" => {}
        other => {
            result.add_error(format!(
                "Security profile '{}' is invalid. Use: open, balanced, hardened, paranoid (or maximum as an alias for paranoid)",
                other
            ));
        }
    }
}

/// Pre-parse validation: check TOML syntax without full deserialization.
pub fn validate_toml_syntax(raw: &str) -> Result<()> {
    // Try to parse as generic TOML value first
    let _: toml::Value = toml::from_str(raw).context("TOML syntax error in config file")?;

    Ok(())
}

/// Full config validation including syntax and semantics.
pub fn validate_config_file(path: &std::path::PathBuf) -> Result<ValidationResult> {
    // First check TOML syntax
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file: {}", path.display()))?;

    validate_toml_syntax(&raw)
        .with_context(|| format!("validating TOML syntax: {}", path.display()))?;

    // Then try to parse as CalciforgeConfig
    let config: CalciforgeConfig =
        toml::from_str(&raw).with_context(|| format!("parsing config file: {}", path.display()))?;

    // Run semantic validation
    let result = validate_config(&config);

    Ok(result)
}

#[cfg(test)]
#[path = "validator_test_support.rs"]
mod validator_test_support;
#[cfg(test)]
#[path = "validator_tests_1.rs"]
mod validator_tests_1;
#[cfg(test)]
#[path = "validator_tests_2.rs"]
mod validator_tests_2;
#[cfg(test)]
#[path = "validator_tests_3.rs"]
mod validator_tests_3;
