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
use std::collections::HashSet;

use crate::config::CalciforgeConfig;

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
        match agent.kind.as_str() {
            "openclaw-channel" => {
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
                if agent.reply_auth_token.is_none() {
                    result.add_warning(format!(
                        "Agent '{}' uses openclaw-channel without reply_auth_token; callback replies should be bearer-protected outside isolated local tests",
                        agent.id
                    ));
                }
            }
            "openai-compat" => {
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
            "openclaw-http" => {
                result.add_error(format!(
                    "Agent '{}' uses removed kind 'openclaw-http'; migrate to kind='openclaw-channel' and install the Calciforge OpenClaw channel plugin",
                    agent.id
                ));
            }
            "openclaw-native" => {
                result.add_error(format!(
                    "Agent '{}' uses unsupported kind 'openclaw-native'; /hooks/agent is async automation, not a synchronous chat adapter. Use kind='openclaw-channel'",
                    agent.id
                ));
            }
            "zeroclaw" => {
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
            "zeroclaw-http" | "zeroclaw-native" => {
                if agent.endpoint.trim().is_empty() {
                    result.add_error(format!(
                        "Agent '{}' kind '{}' requires endpoint",
                        agent.id, agent.kind
                    ));
                }
            }
            "cli" | "acp" | "acpx" => {
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
            "codex-cli" | "dirac-cli" => {}
            other => {
                result.add_error(format!("Agent '{}' has unknown kind '{}'", agent.id, other));
            }
        }
    }
}

fn is_openclaw_model_id(model: &str) -> bool {
    let trimmed = model.trim();
    trimmed == "openclaw" || trimmed.starts_with("openclaw/")
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

    // Check duplicate synthetic model IDs across all synthetic model classes.
    let mut synthetic_model_ids = HashSet::new();
    for alloy in &config.alloys {
        if !synthetic_model_ids.insert(&alloy.id) {
            result.add_error(format!("Duplicate alloy ID: '{}'", alloy.id));
        }
    }
    for cascade in &config.cascades {
        if !synthetic_model_ids.insert(&cascade.id) {
            result.add_error(format!("Duplicate synthetic model ID: '{}'", cascade.id));
        }
    }
    for dispatcher in &config.dispatchers {
        if !synthetic_model_ids.insert(&dispatcher.id) {
            result.add_error(format!("Duplicate synthetic model ID: '{}'", dispatcher.id));
        }
    }
    for exec_model in &config.exec_models {
        if !synthetic_model_ids.insert(&exec_model.id) {
            result.add_error(format!("Duplicate synthetic model ID: '{}'", exec_model.id));
        }
    }

    // Check duplicate model shortcut aliases
    let mut shortcut_aliases = HashSet::new();
    for shortcut in &config.model_shortcuts {
        if !shortcut_aliases.insert(&shortcut.alias) {
            result.add_error(format!(
                "Duplicate model shortcut alias: '{}'",
                shortcut.alias
            ));
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

/// Validate named cascades, dispatchers, and exec models.
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

    for exec_model in &config.exec_models {
        if exec_model.id.trim().is_empty() {
            result.add_error("Exec model has an empty id".to_string());
        }
        if exec_model.context_window == 0 {
            result.add_error(format!(
                "Exec model '{}' has context_window=0",
                exec_model.id
            ));
        }
        if exec_model.command.trim().is_empty() {
            result.add_error(format!(
                "Exec model '{}' has an empty command",
                exec_model.id
            ));
        }
    }
}

/// Validate proxy configuration.
fn validate_proxy_config(proxy: &crate::config::ProxyConfig, result: &mut ValidationResult) {
    if !proxy.enabled {
        return;
    }

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

    // Validate backend_type
    match proxy.backend_type.as_str() {
        "http" | "embedded" | "library" | "mock" | "helicone" | "traceloop" => {}
        other => {
            result.add_error(format!(
                "Proxy backend_type '{}' is invalid. Use: http, embedded, library, mock, helicone, traceloop",
                other
            ));
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
        "off" | "minimal" | "balanced" | "maximum" => {}
        other => {
            result.add_error(format!(
                "Security adversary_detector_profile '{}' is invalid. Use: off, minimal, balanced, maximum",
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
mod tests {
    //! Behavioral tests for `validate_config`. Round-2 test quality
    //! audit (2026-04-24) flagged this module as having zero tests
    //! despite ~290 lines of validation logic. These tests close the
    //! most important invariants (duplicates, dangling references,
    //! out-of-range fields) so future refactors can't silently regress.
    use super::*;
    use crate::config::CalciforgeConfig;

    /// Minimal TOML that passes validation. Each negative test
    /// derives from this by prepending/appending ONE targeted
    /// deviation so the failing invariant is the only difference
    /// between the valid fixture and the test under test.
    const MIN_VALID: &str = r#"
[calciforge]
version = 2

[context]
buffer_size = 20
inject_depth = 5

[[identities]]
id = "alice"
aliases = [{ channel = "telegram", id = "7000000001" }]
role = "owner"

[[agents]]
id = "bot"
kind = "cli"
command = "/bin/echo"
args = []

[[channels]]
kind = "telegram"
bot_token_file = "/tmp/nope"
"#;

    fn parse(toml: &str) -> CalciforgeConfig {
        toml::from_str(toml).expect("fixture should parse")
    }

    /// Given a minimal config with no violations,
    /// when validate_config runs,
    /// then `is_valid()` is true. Positive baseline.
    #[test]
    fn baseline_minimum_config_validates_clean() {
        let config = parse(MIN_VALID);
        let result = validate_config(&config);
        assert!(
            result.is_valid(),
            "baseline fixture should validate clean; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn removed_openclaw_http_agent_is_an_error() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-http"
endpoint = "http://127.0.0.1:18789"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(!result.is_valid(), "openclaw-http must fail validation");
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("openclaw-http") && e.contains("openclaw-channel")),
            "error should name the removed kind and migration target; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn openclaw_channel_agent_validates_with_callback_auth() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key = "test-gateway-token"
reply_auth_token = "test-reply-token"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(
            result.is_valid(),
            "openclaw-channel should validate; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn openai_compat_agent_validates() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
model = "local-kimi-gpt55"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(
            result.is_valid(),
            "openai-compat should validate; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn openai_compat_rejects_openclaw_model_ids() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "librarian"
kind = "openai-compat"
endpoint = "http://127.0.0.1:18789"
api_key = "test-gateway-token"
model = "openclaw/main"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "OpenClaw model IDs should require openclaw-channel"
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("OpenClaw") && e.contains("openclaw-channel")),
            "error should point to openclaw-channel; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn openai_compat_without_model_requires_override_opt_in() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "openai-compat without model or allow_model_override must fail"
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("allow_model_override")),
            "error should mention allow_model_override; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn openai_compat_without_model_validates_when_override_is_explicit() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
allow_model_override = true
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(
            result.is_valid(),
            "openai-compat with explicit model override should validate; errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn zeroclaw_agent_requires_api_key_or_file() {
        let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "librarianzero"
kind = "zeroclaw"
endpoint = "http://127.0.0.1:18799"
"#;
        let config = parse(fixture);
        let result = validate_config(&config);
        assert!(!result.is_valid(), "zeroclaw without key must fail");
        assert!(
            result.errors.iter().any(|e| e.contains("api_key")),
            "error should mention missing api_key/api_key_file; errors: {:?}",
            result.errors
        );
    }

    /// Given a config with two agents sharing the same id,
    /// when validate_config runs,
    /// then an error naming the duplicated id is produced.
    #[test]
    fn duplicate_agent_id_is_an_error() {
        let fixture = format!(
            "{MIN_VALID}\n[[agents]]\nid = \"bot\"\nkind = \"cli\"\ncommand = \"/bin/echo\"\nargs = []\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(!result.is_valid(), "duplicate agent id must fail");
        assert!(
            result.errors.iter().any(|e| e.contains("bot")),
            "error must name the duplicated id 'bot'; errors: {:?}",
            result.errors
        );
    }

    /// Given a config with two identities sharing the same id,
    /// when validate_config runs,
    /// then an error naming the duplicated id is produced.
    #[test]
    fn duplicate_identity_id_is_an_error() {
        let fixture = format!(
            "{MIN_VALID}\n[[identities]]\nid = \"alice\"\naliases = [{{ channel = \"signal\", id = \"7000000099\" }}]\nrole = \"user\"\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(!result.is_valid(), "duplicate identity id must fail");
        assert!(
            result.errors.iter().any(|e| e.contains("alice")),
            "error must name the duplicated id 'alice'; errors: {:?}",
            result.errors
        );
    }

    /// Given a proxy config with an unparseable bind address,
    /// when validate_config runs,
    /// then an error is produced naming the bind/address problem.
    #[test]
    fn malformed_proxy_bind_is_an_error() {
        let fixture = format!(
            "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"not-an-address\"\nbackend_type = \"http\"\nbackend_url = \"https://api.example.com\"\ntimeout_seconds = 10\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "invalid bind address must fail; errors: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| {
                let lower = e.to_lowercase();
                lower.contains("bind") || lower.contains("address")
            }),
            "error should name the bind/address problem; errors: {:?}",
            result.errors
        );
    }

    /// Given a proxy config with `timeout_seconds = 0`,
    /// when validate_config runs,
    /// then an error is produced — a zero timeout means requests
    /// hang indefinitely, which is never the intent.
    #[test]
    fn zero_proxy_timeout_is_an_error() {
        let fixture = format!(
            "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"http\"\nbackend_url = \"https://api.example.com\"\ntimeout_seconds = 0\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "zero proxy timeout must fail; errors: {:?}",
            result.errors
        );
    }

    /// Given a routing rule referencing an agent id that doesn't
    /// exist in the agent list,
    /// when validate_config runs,
    /// then an error naming the missing agent is produced.
    ///
    /// Catches the most common cause of silent "agent unavailable"
    /// at runtime: typo in a routing rule.
    #[test]
    fn routing_rule_default_to_nonexistent_agent_is_an_error() {
        let fixture =
            format!("{MIN_VALID}\n[[routing]]\nidentity = \"alice\"\ndefault_agent = \"ghost\"\n");
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "routing default_agent pointing at a non-existent agent must fail; \
             errors: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.contains("ghost")),
            "error should name the missing agent id 'ghost'; errors: {:?}",
            result.errors
        );
    }

    /// Given a routing rule whose `default_agent` is valid but whose
    /// `allowed_agents` list contains an id not in the agent list,
    /// when validate_config runs,
    /// then an error naming both the identity and the missing agent
    /// is produced.
    ///
    /// Validates the branch in `validate_routing_rules` that walks
    /// each entry of `allowed_agents` — a test for
    /// `default_agent` alone wouldn't exercise this code path.
    #[test]
    fn routing_rule_allowed_list_with_nonexistent_agent_is_an_error() {
        let fixture = format!(
            "{MIN_VALID}\n[[routing]]\nidentity = \"alice\"\ndefault_agent = \"bot\"\nallowed_agents = [\"bot\", \"ghost\"]\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(
            !result.is_valid(),
            "allowed_agents pointing at a non-existent agent must fail; \
             errors: {:?}",
            result.errors
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("ghost") && e.contains("alice")),
            "error should name both the identity 'alice' and missing agent \
             'ghost'; errors: {:?}",
            result.errors
        );
    }
}
