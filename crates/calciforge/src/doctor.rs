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
    check_secret_tooling(&mut report);
    check_proxy_environment(&mut report);
    check_agent_proxy_coverage(&config, &proxy_environment_from_process(), &mut report);
    check_agent_wiring(&config, no_network, &mut report).await;
    check_persisted_state(&config, &mut report);

    Ok(report)
}

fn check_secret_tooling(report: &mut DoctorReport) {
    match which("fnox") {
        Some(path) => report.ok(format!("fnox found at {}", path.display())),
        None => report.warn(
            "fnox not found in PATH; env and Vaultwarden secrets may still work, \
             but fnox-backed discovery/substitution will fail",
        ),
    }

    match which("mcp-server") {
        Some(path) => report.ok(format!(
            "calciforge secret MCP server found at {}",
            path.display()
        )),
        None => report.warn(
            "mcp-server not found in PATH; agents will not get Calciforge MCP \
             secret-name discovery unless configured with an absolute path",
        ),
    }

    match which("calciforge-secrets") {
        Some(path) => report.ok(format!(
            "calciforge-secrets CLI found at {}",
            path.display()
        )),
        None => report.warn(
            "calciforge-secrets CLI not found in PATH; non-MCP secret-name discovery \
             is unavailable",
        ),
    }
}

#[derive(Debug, Clone, Default)]
struct ProxyEnvironment {
    http: Option<String>,
    https: Option<String>,
    no_proxy: Option<String>,
}

fn check_proxy_environment(report: &mut DoctorReport) {
    check_proxy_environment_in(proxy_environment_from_process(), report);
}

fn proxy_environment_from_process() -> ProxyEnvironment {
    ProxyEnvironment {
        http: std::env::var("HTTP_PROXY")
            .ok()
            .or_else(|| std::env::var("http_proxy").ok()),
        https: std::env::var("HTTPS_PROXY")
            .ok()
            .or_else(|| std::env::var("https_proxy").ok()),
        no_proxy: std::env::var("NO_PROXY")
            .ok()
            .or_else(|| std::env::var("no_proxy").ok()),
    }
}

fn check_proxy_environment_in(env: ProxyEnvironment, report: &mut DoctorReport) {
    match (env.http, env.https) {
        (Some(http), Some(https)) => {
            if http == https {
                report.ok(format!(
                    "HTTP(S)_PROXY configured: {}",
                    display_proxy_value(&http)
                ));
            } else {
                report.warn(format!(
                    "HTTP_PROXY and HTTPS_PROXY differ; HTTP_PROXY={}, HTTPS_PROXY={}",
                    display_proxy_value(&http),
                    display_proxy_value(&https)
                ));
            }
        }
        (Some(http), None) => report.warn(format!(
            "HTTP_PROXY is set ({}) but HTTPS_PROXY is not; HTTPS agent traffic may bypass security-proxy",
            display_proxy_value(&http)
        )),
        (None, Some(https)) => report.warn(format!(
            "HTTPS_PROXY is set ({}) but HTTP_PROXY is not; HTTP agent traffic may bypass security-proxy",
            display_proxy_value(&https)
        )),
        (None, None) => report.warn(
            "HTTP_PROXY/HTTPS_PROXY are not set in this process; subprocess agents \
             may bypass security-proxy unless OS/network enforcement is active",
        ),
    }

    let no_proxy = env.no_proxy.unwrap_or_default();
    if no_proxy.contains("127.0.0.1") || no_proxy.contains("localhost") {
        report.ok("NO_PROXY includes local loopback");
    } else {
        report.warn("NO_PROXY does not include localhost/127.0.0.1; local health calls may loop through security-proxy");
    }
}

fn check_agent_proxy_coverage(
    config: &CalciforgeConfig,
    env: &ProxyEnvironment,
    report: &mut DoctorReport,
) {
    let subprocess_agents = config
        .agents
        .iter()
        .filter(|agent| is_subprocess_agent(agent))
        .collect::<Vec<_>>();
    let subprocess_count = subprocess_agents.len();
    let proxy_bind = config
        .proxy
        .as_ref()
        .filter(|proxy| proxy.enabled)
        .map(|proxy| proxy.bind.as_str());
    let external_count = config
        .agents
        .iter()
        .filter(|agent| is_external_agent_daemon(agent, proxy_bind))
        .count();

    if subprocess_count > 0 {
        if has_http_and_https_proxy(env) {
            let override_count = subprocess_agents
                .iter()
                .filter(|agent| has_agent_proxy_env_override(agent))
                .count();
            let clearing_count = subprocess_agents
                .iter()
                .filter(|agent| clears_agent_proxy_env(agent))
                .count();

            if clearing_count > 0 {
                report.warn(format!(
                    "{clearing_count} subprocess agent(s) set empty proxy env values; CLI/exec agents may bypass security-proxy"
                ));
            } else if override_count > 0 {
                report.warn(format!(
                    "{override_count} subprocess agent(s) define agent-level proxy env overrides; verify they still route through security-proxy"
                ));
            } else {
                report.ok(format!(
                    "{subprocess_count} subprocess agent(s) will inherit HTTP_PROXY/HTTPS_PROXY from Calciforge"
                ));
            }
        } else {
            report.warn(format!(
                "{subprocess_count} subprocess agent(s) configured, but Calciforge lacks complete HTTP_PROXY/HTTPS_PROXY; CLI/exec agents may bypass security-proxy"
            ));
        }
    }

    if external_count > 0 {
        report.warn(format!(
            "{external_count} externally managed HTTP/native agent endpoint(s) configured; doctor cannot verify their process proxy environment"
        ));
    }
}

fn has_http_and_https_proxy(env: &ProxyEnvironment) -> bool {
    env.http
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && env
            .https
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn display_proxy_value(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.to_string();
    };

    if url.username().is_empty() && url.password().is_none() {
        return value.to_string();
    }

    let _ = url.set_username("redacted");
    let _ = url.set_password(Some("redacted"));
    url.to_string()
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        executable_candidates(&dir, bin)
            .into_iter()
            .find(|candidate| is_executable_file(candidate))
    })
}

#[cfg(windows)]
fn executable_candidates(dir: &Path, bin: &str) -> Vec<PathBuf> {
    if Path::new(bin).extension().is_some() {
        return vec![dir.join(bin)];
    }

    let pathext = std::env::var_os("PATHEXT")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());

    pathext
        .split(';')
        .filter(|ext| !ext.trim().is_empty())
        .map(|ext| dir.join(format!("{bin}{ext}")))
        .collect()
}

#[cfg(not(windows))]
fn executable_candidates(dir: &Path, bin: &str) -> Vec<PathBuf> {
    vec![dir.join(bin)]
}

fn is_executable_file(candidate: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(candidate) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
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
            | "dirac-cli"
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

fn is_subprocess_agent(agent: &AgentConfig) -> bool {
    matches!(
        agent.kind.as_str(),
        "cli" | "codex-cli" | "dirac-cli" | "acp" | "acpx"
    )
}

fn has_agent_proxy_env_override(agent: &AgentConfig) -> bool {
    agent
        .env
        .as_ref()
        .is_some_and(|env| env.keys().any(|key| is_proxy_env_key(key)))
}

fn clears_agent_proxy_env(agent: &AgentConfig) -> bool {
    agent.env.as_ref().is_some_and(|env| {
        env.iter()
            .any(|(key, value)| is_proxy_env_key(key) && value.trim().is_empty())
    })
}

fn is_proxy_env_key(key: &str) -> bool {
    matches!(
        key,
        "HTTP_PROXY" | "http_proxy" | "HTTPS_PROXY" | "https_proxy"
    )
}

fn is_external_agent_daemon(agent: &AgentConfig, proxy_bind: Option<&str>) -> bool {
    is_http_agent(agent)
        && !proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind))
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

    #[test]
    fn proxy_environment_warns_when_missing() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("HTTP_PROXY/HTTPS_PROXY are not set")
        }));
    }

    #[test]
    fn proxy_environment_accepts_matching_http_and_https_proxy() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok && finding.message.contains("HTTP(S)_PROXY configured")
        }));
    }

    #[test]
    fn proxy_environment_redacts_credentials() {
        let proxy = format!(
            "{}://{}:{}@{}",
            "http", "user", "pass", "proxy.example:8080"
        );
        assert_eq!(
            display_proxy_value(&proxy),
            format!(
                "{}://{}:{}@{}/",
                "http", "redacted", "redacted", "proxy.example:8080"
            )
        );
    }

    #[test]
    fn subprocess_agent_proxy_coverage_warns_without_complete_proxy_env() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("CLI/exec agents may bypass security-proxy")
        }));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_accepts_complete_proxy_env() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "dirac".to_string(),
            kind: "dirac-cli".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding
                    .message
                    .contains("will inherit HTTP_PROXY/HTTPS_PROXY")
        }));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_warns_when_agent_env_overrides_proxy() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            env: Some(HashMap::from([(
                "HTTPS_PROXY".to_string(),
                "http://127.0.0.1:9999".to_string(),
            )])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("agent-level proxy env overrides")
        }));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_warns_when_agent_env_clears_proxy() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            env: Some(HashMap::from([("HTTP_PROXY".to_string(), String::new())])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("set empty proxy env values")
        }));
    }

    #[test]
    fn external_agent_proxy_coverage_warns_that_daemon_env_is_unverified() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "openclaw".to_string(),
            kind: "openclaw-native".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("doctor cannot verify their process proxy environment")
        }));
    }

    #[test]
    fn disabled_proxy_bind_does_not_hide_external_daemon_warning() {
        let mut config = base_config();
        if let Some(proxy) = &mut config.proxy {
            proxy.enabled = false;
        }
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("doctor cannot verify their process proxy environment")
        }));
    }

    #[test]
    fn local_model_gateway_agent_does_not_count_as_external_daemon() {
        let config = base_config();
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
            },
            &mut report,
        );

        assert!(!report.findings.iter().any(|finding| finding
            .message
            .contains("doctor cannot verify their process proxy environment")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_detection_rejects_non_executable_regular_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("fnox");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write file");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&path, permissions).unwrap();

        assert!(!is_executable_file(&path));

        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        assert!(is_executable_file(&path));
    }
}
