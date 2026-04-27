//! Runtime/config doctor checks for Calciforge deployments.
//!
//! The doctor is intentionally conservative: it reports actionable deployment
//! problems without printing tokens, secret values, or channel identifiers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::config::{self, AgentConfig, CalciforgeConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Error,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug)]
struct Finding {
    severity: Severity,
    message: String,
}

#[derive(Debug, Default)]
pub struct DoctorReport {
    findings: Vec<Finding>,
}

impl DoctorReport {
    fn push(&mut self, severity: Severity, message: impl Into<String>) {
        self.findings.push(Finding {
            severity,
            message: message.into(),
        });
    }

    fn ok(&mut self, message: impl Into<String>) {
        self.push(Severity::Ok, message);
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.push(Severity::Warn, message);
    }

    fn error(&mut self, message: impl Into<String>) {
        self.push(Severity::Error, message);
    }

    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }

    pub fn print(&self) {
        println!("Calciforge doctor:");
        for finding in &self.findings {
            println!("  [{:5}] {}", finding.severity.label(), finding.message);
        }
    }
}

pub async fn run(config_path: &Path, no_network: bool) -> Result<DoctorReport> {
    let mut report = DoctorReport::default();

    match config::validator::validate_config_file(&config_path.to_path_buf()) {
        Ok(validation) if validation.is_valid() => {
            report.ok(format!(
                "config parses and validates: {}",
                config_path.display()
            ));
            for warning in validation.warnings {
                report.warn(format!("config warning: {warning}"));
            }
        }
        Ok(validation) => {
            for error in validation.errors {
                report.error(format!("config validation error: {error}"));
            }
            for warning in validation.warnings {
                report.warn(format!("config warning: {warning}"));
            }
            return Ok(report);
        }
        Err(err) => {
            report.error(format!(
                "failed to validate config {}: {err}",
                config_path.display()
            ));
            return Ok(report);
        }
    }

    let config = match config::load_config_from(&config_path.to_path_buf()) {
        Ok(config) => config,
        Err(err) => {
            report.error(format!("failed to load config after validation: {err}"));
            return Ok(report);
        }
    };

    report.ok(format!(
        "{} identities, {} agents, {} channels configured",
        config.identities.len(),
        config.agents.len(),
        config.channels.len()
    ));

    check_secret_files(&config, &mut report);
    check_agent_wiring(&config, no_network, &mut report).await;
    check_persisted_state(&config, &mut report);

    Ok(report)
}

fn check_secret_files(config: &CalciforgeConfig, report: &mut DoctorReport) {
    for agent in &config.agents {
        if agent.api_key.is_some() || agent.auth_token.is_some() {
            report.warn(format!(
                "agent '{}' stores an inline token; prefer api_key_file",
                agent.id
            ));
        }
        if let Some(path) = &agent.api_key_file {
            check_readable_file(
                report,
                "agent api_key_file",
                &agent.id,
                &path.to_string_lossy(),
            );
        }
    }

    for channel in &config.channels {
        if !channel.enabled {
            continue;
        }
        if let Some(path) = &channel.bot_token_file {
            check_readable_file(report, "channel bot_token_file", &channel.kind, path);
        }
    }

    if let Some(proxy) = &config.proxy {
        if proxy.api_key.is_some() {
            report.warn("proxy stores an inline api_key; prefer api_key_file");
        }
        if let Some(path) = &proxy.api_key_file {
            check_readable_file(
                report,
                "proxy api_key_file",
                "proxy",
                &path.to_string_lossy(),
            );
        }
        if proxy.backend_api_key.is_some() {
            report.warn("proxy backend stores an inline api_key; prefer backend_api_key_file");
        }
        if let Some(path) = &proxy.backend_api_key_file {
            check_readable_file(
                report,
                "proxy backend_api_key_file",
                "proxy",
                &path.to_string_lossy(),
            );
        }
        for provider in &proxy.providers {
            if provider.api_key.is_some() {
                report.warn(format!(
                    "proxy provider '{}' stores an inline api_key; prefer api_key_file",
                    provider.id
                ));
            }
            if let Some(path) = &provider.api_key_file {
                check_readable_file(
                    report,
                    "proxy provider api_key_file",
                    &provider.id,
                    &path.to_string_lossy(),
                );
            }
        }
    }
}

fn check_readable_file(report: &mut DoctorReport, label: &str, owner: &str, path: &str) {
    let expanded = config::expand_tilde(path);
    match std::fs::metadata(&expanded) {
        Ok(metadata) if metadata.is_file() => {
            if std::fs::File::open(&expanded).is_ok() {
                report.ok(format!("{label} for '{owner}' is readable"));
            } else {
                report.error(format!("{label} for '{owner}' exists but is not readable"));
            }
        }
        Ok(_) => report.error(format!("{label} for '{owner}' is not a regular file")),
        Err(err) => report.error(format!("{label} for '{owner}' is not readable: {err}")),
    }
}

async fn check_agent_wiring(
    config: &CalciforgeConfig,
    no_network: bool,
    report: &mut DoctorReport,
) {
    let proxy_bind = config.proxy.as_ref().map(|proxy| proxy.bind.as_str());
    let mut endpoint_counts: HashMap<&str, usize> = HashMap::new();

    for agent in &config.agents {
        if !is_known_agent_kind(&agent.kind) {
            report.error(format!(
                "agent '{}' has unknown kind '{}'",
                agent.id, agent.kind
            ));
        }

        if is_http_agent(agent) {
            if agent.endpoint.trim().is_empty() {
                report.error(format!(
                    "agent '{}' kind '{}' requires endpoint",
                    agent.id, agent.kind
                ));
                continue;
            }
            *endpoint_counts.entry(agent.endpoint.as_str()).or_default() += 1;

            if agent.kind == "openclaw-http"
                && proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind))
                && agent.id != "gateway"
            {
                report.warn(format!(
                    "agent '{}' points at the local Calciforge proxy; use a clearly named raw gateway agent or route to the real downstream agent",
                    agent.id
                ));
            }

            if !no_network {
                check_endpoint_reachable(agent, report).await;
            }
        }
    }

    for (endpoint, count) in endpoint_counts {
        if count > 1 {
            report.warn(format!(
                "{count} agents share endpoint {endpoint}; verify these are distinct lanes rather than stale copy/paste"
            ));
        }
    }
}

fn is_known_agent_kind(kind: &str) -> bool {
    matches!(
        kind,
        "openclaw-http"
            | "openclaw-native"
            | "openclaw-channel"
            | "zeroclaw-http"
            | "zeroclaw-native"
            | "zeroclaw"
            | "cli"
            | "codex-cli"
            | "acp"
            | "acpx"
    )
}

fn is_http_agent(agent: &AgentConfig) -> bool {
    matches!(
        agent.kind.as_str(),
        "openclaw-http"
            | "openclaw-native"
            | "openclaw-channel"
            | "zeroclaw-http"
            | "zeroclaw-native"
            | "zeroclaw"
    )
}

async fn check_endpoint_reachable(agent: &AgentConfig, report: &mut DoctorReport) {
    let Ok(url) = reqwest::Url::parse(&agent.endpoint) else {
        report.error(format!(
            "agent '{}' endpoint is not a valid URL: {}",
            agent.id, agent.endpoint
        ));
        return;
    };

    let Some(host) = url.host_str() else {
        report.error(format!("agent '{}' endpoint has no host", agent.id));
        return;
    };
    let Some(port) = url.port_or_known_default() else {
        report.error(format!("agent '{}' endpoint has no TCP port", agent.id));
        return;
    };

    let target = format!("{host}:{port}");
    match timeout(Duration::from_millis(800), TcpStream::connect(&target)).await {
        Ok(Ok(_)) => report.ok(format!("agent '{}' endpoint accepts TCP", agent.id)),
        Ok(Err(err)) => report.error(format!(
            "agent '{}' endpoint is unreachable at {target}: {err}",
            agent.id
        )),
        Err(_) => report.error(format!(
            "agent '{}' endpoint timed out at {target}",
            agent.id
        )),
    }
}

fn endpoint_matches_bind(endpoint: &str, bind: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    let Some(endpoint_port) = url.port_or_known_default() else {
        return false;
    };
    let Some((bind_host, bind_port)) = split_bind(bind) else {
        return false;
    };
    if endpoint_port != bind_port {
        return false;
    }
    let endpoint_host = url.host_str().unwrap_or_default();
    is_equivalent_local_host(endpoint_host, bind_host)
}

fn split_bind(bind: &str) -> Option<(&str, u16)> {
    let (host, port) = bind.rsplit_once(':')?;
    Some((host.trim_matches(['[', ']']), port.parse().ok()?))
}

fn is_equivalent_local_host(endpoint_host: &str, bind_host: &str) -> bool {
    endpoint_host == bind_host
        || matches!(bind_host, "0.0.0.0" | "::")
        || matches!(endpoint_host, "localhost" | "127.0.0.1" | "::1")
            && matches!(bind_host, "localhost" | "127.0.0.1" | "::1")
}

fn check_persisted_state(config: &CalciforgeConfig, report: &mut DoctorReport) {
    let state_dir = default_state_dir();
    check_persisted_state_in(config, &state_dir, report);
}

fn check_persisted_state_in(
    config: &CalciforgeConfig,
    state_dir: &Path,
    report: &mut DoctorReport,
) {
    let agent_ids: HashSet<&str> = config
        .agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect();
    let synthetic_ids = synthetic_model_ids(config);

    let active_agents_path = state_dir.join("active-agents.json");
    if let Ok(map) = read_state_map(&active_agents_path) {
        for (identity, agent_id) in map {
            if agent_ids.contains(agent_id.as_str()) {
                report.ok(format!(
                    "active agent for '{identity}' points to '{agent_id}'"
                ));
            } else {
                report.error(format!(
                    "active agent for '{identity}' points to unknown agent '{agent_id}'"
                ));
            }
        }
    }

    let active_models_path = state_dir.join("active-models.json");
    if let Ok(map) = read_state_map(&active_models_path) {
        for (identity, model_id) in map {
            if synthetic_ids.contains(model_id.as_str()) {
                report.ok(format!(
                    "active model override for '{identity}' points to '{model_id}'"
                ));
            } else {
                report.error(format!(
                    "active model override for '{identity}' points to unknown synthetic model '{model_id}'"
                ));
            }
        }
    }
}

fn default_state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".calciforge").join("state")
}

fn read_state_map(path: &Path) -> Result<HashMap<String, String>, ()> {
    let text = std::fs::read_to_string(path).map_err(|_| ())?;
    serde_json::from_str(&text).map_err(|_| ())
}

fn synthetic_model_ids(config: &CalciforgeConfig) -> HashSet<&str> {
    config
        .alloys
        .iter()
        .map(|model| model.id.as_str())
        .chain(config.cascades.iter().map(|model| model.id.as_str()))
        .chain(config.dispatchers.iter().map(|model| model.id.as_str()))
        .chain(config.exec_models.iter().map(|model| model.id.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CalciforgeHeader, ProxyConfig, RoutingRule, SyntheticModelConfig};

    fn base_config() -> CalciforgeConfig {
        CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![],
            agents: vec![
                AgentConfig {
                    id: "gateway".to_string(),
                    kind: "openclaw-http".to_string(),
                    endpoint: "http://127.0.0.1:18083".to_string(),
                    model: Some("local-kimi-gpt55".to_string()),
                    api_key_file: Some(PathBuf::from("/tmp/nonexistent-test-token")),
                    ..Default::default()
                },
                AgentConfig {
                    id: "custodian".to_string(),
                    kind: "openclaw-http".to_string(),
                    endpoint: "http://127.0.0.1:18083".to_string(),
                    model: Some("local-kimi-gpt55".to_string()),
                    api_key_file: Some(PathBuf::from("/tmp/nonexistent-test-token")),
                    ..Default::default()
                },
            ],
            routing: vec![RoutingRule {
                identity: "brian".to_string(),
                default_agent: "gateway".to_string(),
                allowed_agents: vec!["gateway".to_string(), "custodian".to_string()],
            }],
            proxy: Some(ProxyConfig {
                enabled: true,
                bind: "127.0.0.1:18083".to_string(),
                ..Default::default()
            }),
            dispatchers: vec![crate::config::DispatcherConfig {
                id: "local-kimi-gpt55".to_string(),
                name: Some("Local Kimi then GPT".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "kimi-for-coding".to_string(),
                    context_window: 128_000,
                }],
            }],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            alloys: vec![],
            cascades: vec![],
            exec_models: vec![],
            security: None,
            local_models: None,
        }
    }

    #[test]
    fn detects_non_gateway_agent_pointing_at_local_proxy() {
        let config = base_config();
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("agent 'custodian' points at the local Calciforge proxy")
        }));
    }

    #[test]
    fn validates_persisted_active_state_against_config() {
        let config = base_config();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("active-agents.json"),
            r#"{"brian":"missing-agent"}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("active-models.json"),
            r#"{"brian":"missing-model"}"#,
        )
        .unwrap();

        let mut report = DoctorReport::default();
        check_persisted_state_in(&config, tmp.path(), &mut report);

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("unknown agent 'missing-agent'")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("unknown synthetic model 'missing-model'")
        }));
    }

    #[test]
    fn recognizes_local_proxy_endpoint_equivalence() {
        assert!(endpoint_matches_bind(
            "http://127.0.0.1:18083",
            "127.0.0.1:18083"
        ));
        assert!(endpoint_matches_bind(
            "http://localhost:18083",
            "127.0.0.1:18083"
        ));
        assert!(endpoint_matches_bind(
            "http://127.0.0.1:18083",
            "0.0.0.0:18083"
        ));
        assert!(!endpoint_matches_bind(
            "http://127.0.0.1:18793",
            "127.0.0.1:18083"
        ));
    }
}
