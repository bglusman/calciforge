//! Runtime/config doctor checks for Calciforge deployments.
//!
//! The doctor is intentionally conservative: it reports actionable deployment
//! problems without printing tokens, secret values, or channel identifiers.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use adversary_detector::{
    AdversaryScanner, ScanContext, ScanVerdict, ScannerCheckConfig, ScannerConfig,
};
use anyhow::{Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::{TcpStream, UdpSocket};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::adapters::{
    agent_supports_model_override, find_executable_for_agent, subprocess_command_for_agent,
};
use crate::agent_kinds::{
    AgentKind, AgentKindLifecycle, agent_kind_metadata, known_agent_kind_names, parse_agent_kind,
};
use crate::config::{self, AgentConfig, CalciforgeConfig};
use crate::model_names::configured_first_class_model_ids;
use crate::providers::alloy::AlloyManager;
use crate::proxy::model_resolver::ModelResolver;
use crate::proxy::routing;

mod agent_adapter_doctor;
mod security_proxy_runtime;

const DOCTOR_REQUIRE_AGENT_EGRESS_PROXY_ENV: &str = "CALCIFORGE_DOCTOR_REQUIRE_AGENT_EGRESS_PROXY";

#[derive(Debug, Clone, Copy, Default)]
pub struct DoctorOptions {
    pub no_network: bool,
    pub require_agent_egress_proxy: Option<bool>,
}

pub fn require_agent_egress_proxy_override_from_env() -> Option<bool> {
    std::env::var(DOCTOR_REQUIRE_AGENT_EGRESS_PROXY_ENV)
        .ok()
        .as_deref()
        .map(truthy_env_value)
}

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

pub async fn run_with_options(config_path: &Path, options: DoctorOptions) -> Result<DoctorReport> {
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
    check_model_gateway_config(&config, &mut report);
    check_secret_tooling(&mut report);
    check_scanner_config(&config, options.no_network, &mut report).await;
    check_proxy_environment(&mut report);
    security_proxy_runtime::check(&mut report).await;
    check_security_proxy_ca_trust(&mut report);
    check_install_node_metadata(options.no_network, &mut report).await;
    let strict_egress_proxy = options
        .require_agent_egress_proxy
        .unwrap_or_else(|| security_requires_agent_egress_proxy(&config));
    check_agent_proxy_coverage_with_strict(
        &config,
        &proxy_environment_from_process(),
        strict_egress_proxy,
        &mut report,
    );
    report_agent_protection_summary(&config, &mut report);
    check_agent_wiring_with_strict(
        &config,
        options.no_network,
        strict_egress_proxy,
        &mut report,
    )
    .await;
    check_persisted_state(&config, &mut report);

    Ok(report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallNodeMetadata {
    name: String,
    host: String,
    user: String,
    ssh_key: Option<PathBuf>,
    os: String,
    install_dir: String,
    config_dir: String,
}

async fn check_install_node_metadata(no_network: bool, report: &mut DoctorReport) {
    let path = install_nodes_state_path();
    if !path.exists() {
        report.ok(format!(
            "no persisted install-node metadata found at {}; remote SSH permission checks skipped",
            path.display()
        ));
        return;
    }

    let nodes = match read_install_nodes(&path) {
        Ok(nodes) => nodes,
        Err(err) => {
            report.error(format!(
                "failed to read install-node metadata {}: {err}",
                path.display()
            ));
            return;
        }
    };

    if nodes.is_empty() {
        report.warn(format!(
            "install-node metadata {} contains no nodes",
            path.display()
        ));
        return;
    }

    if no_network {
        report.ok(format!(
            "{} install-node SSH permission check(s) skipped by --no-network",
            nodes.len()
        ));
        return;
    }

    for node in nodes {
        check_install_node_ssh(&node, report).await;
    }
}

fn install_nodes_state_path() -> PathBuf {
    std::env::var("CALCIFORGE_INSTALL_NODES_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate::config::calciforge_config_home(None).join("install-nodes.json"))
}

fn read_install_nodes(path: &Path) -> Result<Vec<InstallNodeMetadata>> {
    let text = std::fs::read_to_string(path)?;
    parse_install_nodes_json(&text)
}

fn parse_install_nodes_json(text: &str) -> Result<Vec<InstallNodeMetadata>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let nodes = value
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .ok_or_else(|| anyhow::anyhow!("expected top-level object with array field 'nodes'"))?;

    nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let host = json_string(node, "host")
                .filter(|host| !host.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("nodes[{idx}].host is required"))?;
            Ok(InstallNodeMetadata {
                name: json_string(node, "name").unwrap_or_else(|| host.clone()),
                host,
                user: json_string(node, "user").unwrap_or_else(|| "root".to_string()),
                ssh_key: json_string(node, "ssh_key")
                    .filter(|key| !key.trim().is_empty())
                    .map(PathBuf::from),
                os: json_string(node, "os").unwrap_or_else(|| "linux".to_string()),
                install_dir: json_string(node, "install_dir")
                    .unwrap_or_else(|| "/usr/local/bin".to_string()),
                config_dir: json_string(node, "config_dir")
                    .unwrap_or_else(|| "/etc/calciforge".to_string()),
            })
        })
        .collect()
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

async fn check_install_node_ssh(node: &InstallNodeMetadata, report: &mut DoctorReport) {
    let target = format!("{}@{}", node.user, node.host);
    let mut cmd = TokioCommand::new("ssh");
    cmd.kill_on_drop(true);
    cmd.arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=8")
        .arg("-o")
        .arg("BatchMode=yes");
    if let Some(key) = &node.ssh_key {
        cmd.arg("-i").arg(key);
    }
    let permission_command = match remote_install_node_permission_command(node) {
        Ok(command) => command,
        Err(err) => {
            report.error(format!(
                "install node '{}' has invalid SSH permission-check metadata: {err}",
                node.name
            ));
            return;
        }
    };
    cmd.arg(&target).arg(permission_command);

    match timeout(Duration::from_secs(10), cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            report.ok(format!(
                "install node '{}' accepts SSH and allows writes to {} and {}",
                node.name, node.install_dir, node.config_dir
            ));
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            report.error(format!(
                "install node '{}' SSH permission check failed at {}: {}",
                node.name,
                target,
                stderr.trim()
            ));
        }
        Ok(Err(err)) => {
            report.error(format!(
                "install node '{}' SSH permission check could not spawn ssh for {}: {err}",
                node.name, target
            ));
        }
        Err(_) => {
            report.error(format!(
                "install node '{}' SSH permission check timed out at {}",
                node.name, target
            ));
        }
    }
}

fn remote_install_node_permission_command(node: &InstallNodeMetadata) -> Result<String> {
    Ok(format!(
        "set -eu; os={}; install_dir={}; config_dir={}; \
         if [ \"$os\" = linux ] && [ \"$(id -u)\" != 0 ]; then echo 'linux node install requires root SSH for systemd and install paths' >&2; exit 10; fi; \
         for dir in \"$install_dir\" \"$config_dir\"; do test -d \"$dir\" || {{ echo \"missing required directory: $dir\" >&2; exit 11; }}; test -w \"$dir\" || {{ echo \"directory is not writable: $dir\" >&2; exit 12; }}; tmp=\"$dir/.calciforge-doctor-permission-test.$$\"; : > \"$tmp\"; rm -f \"$tmp\"; done; \
         if [ \"$os\" = linux ]; then command -v systemctl >/dev/null 2>&1 || {{ echo 'systemctl not found' >&2; exit 13; }}; test -w /etc/systemd/system || {{ echo '/etc/systemd/system is not writable' >&2; exit 14; }}; fi; \
         echo OK",
        shell_quote_for_remote(&node.os)?,
        shell_quote_for_remote(&node.install_dir)?,
        shell_quote_for_remote(&node.config_dir)?,
    ))
}

fn shell_quote_for_remote(input: &str) -> Result<String> {
    if input.contains(['\0', '\n', '\r']) {
        bail!("remote shell argument contains a control character");
    }
    let escaped = input.replace('\'', "'\\''");
    Ok(format!("'{escaped}'"))
}

fn check_secret_tooling(report: &mut DoctorReport) {
    match which("fnox") {
        Some(path) => {
            report.ok(format!("fnox found at {}", path.display()));
            check_fnox_providers(&path, report);
        }
        None => report.warn(
            "fnox not found in PATH; only env secrets will work, \
             and fnox-backed discovery/substitution will fail",
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

fn check_fnox_providers(fnox_path: &Path, report: &mut DoctorReport) {
    let output = StdCommand::new(fnox_path)
        .args(["provider", "list"])
        .output();
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            report.warn(format!(
                "fnox provider list could not be executed; paste UI and fnox-backed \
                 discovery may fail: {e}"
            ));
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostic = stderr.trim();
        let suffix = if diagnostic.is_empty() {
            String::new()
        } else {
            format!(": {diagnostic}")
        };
        report.warn(format!(
            "fnox provider list failed; paste UI and fnox-backed discovery may fail{suffix}"
        ));
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = count_fnox_provider_lines(&stdout);
    if count == 0 {
        report.warn(
            "fnox has no provider configured; paste UI, !secure set, and fnox-backed \
             secret discovery will fail until a provider is added",
        );
    } else {
        report.ok(format!("fnox has {count} provider(s) configured"));
    }
}

fn count_fnox_provider_lines(stdout: &str) -> usize {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("No providers")
                && !line.starts_with("No provider")
                && !line.starts_with("Providers:")
        })
        .count()
}

#[derive(Debug, Clone, Default)]
struct ProxyEnvironment {
    http: Option<String>,
    https: Option<String>,
    all: Option<String>,
    no_proxy: Option<String>,
    node_extra_ca_certs: Option<String>,
    ssl_cert_file: Option<String>,
    requests_ca_bundle: Option<String>,
    curl_ca_bundle: Option<String>,
    git_ssl_cainfo: Option<String>,
}

fn check_proxy_environment(report: &mut DoctorReport) {
    check_proxy_environment_in(proxy_environment_from_process(), report);
}

fn check_security_proxy_ca_trust(report: &mut DoctorReport) {
    if !cfg!(target_os = "linux") {
        report.ok("Linux system MITM CA trust check skipped on non-Linux host");
        return;
    }

    let Some(ca_cert) = active_security_proxy_ca_cert() else {
        report.ok("security-proxy CA trust check skipped; no active systemd CA env found");
        return;
    };
    let ca_cert = PathBuf::from(ca_cert);
    if !ca_cert.is_file() {
        report.warn(format!(
            "security-proxy CA trust check skipped; active CA file is missing: {}",
            ca_cert.display()
        ));
        return;
    }

    let Some(bundle) = linux_system_ca_bundle_candidates()
        .into_iter()
        .find(|path| path.is_file())
    else {
        report.warn("security-proxy CA trust check skipped; no known Linux system CA bundle found");
        return;
    };

    match verify_ca_cert_with_bundle(&ca_cert, &bundle) {
        Ok(()) => report.ok(format!(
            "Linux system trust accepts active security-proxy CA: {}",
            ca_cert.display()
        )),
        Err(CaTrustVerifyError::OpenSslUnavailable(err)) => report.ok(format!(
            "security-proxy CA trust check skipped; openssl is not available: {err}"
        )),
        Err(CaTrustVerifyError::VerificationFailed(err)) => report.warn(format!(
            "Linux system trust does not accept active security-proxy CA {} via {}: {}. \
             Re-run the installer or refresh the host trust store; tools that use system trust may reject MITM leaf certificates.",
            ca_cert.display(),
            bundle.display(),
            err
        )),
    }
}

fn active_security_proxy_ca_cert() -> Option<String> {
    let process_env = std::env::var("SECURITY_PROXY_CA_CERT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let systemd_envs = active_security_proxy_systemd_envs();
    active_security_proxy_ca_cert_from_values(
        systemd_envs.iter().map(String::as_str),
        process_env.as_deref(),
    )
}

fn active_security_proxy_systemd_envs() -> Vec<String> {
    let mut envs = Vec::new();
    for args in [
        &[
            "--user",
            "show",
            "calciforge-security-proxy.service",
            "-p",
            "Environment",
            "--value",
            "--no-pager",
        ][..],
        &[
            "show",
            "calciforge-security-proxy.service",
            "-p",
            "Environment",
            "--value",
            "--no-pager",
        ][..],
    ] {
        let Ok(output) = StdCommand::new("systemctl").args(args).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !stdout.is_empty() {
            envs.push(stdout);
        }
    }
    envs
}

fn active_security_proxy_ca_cert_from_values<'a>(
    systemd_envs: impl IntoIterator<Item = &'a str>,
    process_env: Option<&str>,
) -> Option<String> {
    systemd_envs
        .into_iter()
        .find_map(|env| environment_value(env, "SECURITY_PROXY_CA_CERT"))
        .or_else(|| {
            process_env
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
        })
}

fn environment_value(env_text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    env_text
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix).map(str::to_string))
        .filter(|value| !value.trim().is_empty())
}

fn linux_system_ca_bundle_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/etc/ssl/certs/ca-certificates.crt"),
        PathBuf::from("/etc/pki/tls/certs/ca-bundle.crt"),
    ]
}

#[derive(Debug, PartialEq, Eq)]
enum CaTrustVerifyError {
    OpenSslUnavailable(String),
    VerificationFailed(String),
}

fn verify_ca_cert_with_bundle(
    ca_cert: &Path,
    bundle: &Path,
) -> std::result::Result<(), CaTrustVerifyError> {
    let output = StdCommand::new("openssl")
        .arg("verify")
        .arg("-CAfile")
        .arg(bundle)
        .arg(ca_cert)
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => CaTrustVerifyError::OpenSslUnavailable(err.to_string()),
            _ => CaTrustVerifyError::VerificationFailed(format!(
                "failed to run openssl verify: {err}"
            )),
        })?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stderr.is_empty() {
        Err(CaTrustVerifyError::VerificationFailed(stdout))
    } else if stdout.is_empty() {
        Err(CaTrustVerifyError::VerificationFailed(stderr))
    } else {
        Err(CaTrustVerifyError::VerificationFailed(format!(
            "{stdout}; {stderr}"
        )))
    }
}

fn proxy_environment_from_process() -> ProxyEnvironment {
    ProxyEnvironment {
        http: std::env::var("HTTP_PROXY")
            .ok()
            .or_else(|| std::env::var("http_proxy").ok()),
        https: std::env::var("HTTPS_PROXY")
            .ok()
            .or_else(|| std::env::var("https_proxy").ok()),
        all: std::env::var("ALL_PROXY")
            .ok()
            .or_else(|| std::env::var("all_proxy").ok()),
        no_proxy: std::env::var("NO_PROXY")
            .ok()
            .or_else(|| std::env::var("no_proxy").ok()),
        node_extra_ca_certs: std::env::var("NODE_EXTRA_CA_CERTS").ok(),
        ssl_cert_file: std::env::var("SSL_CERT_FILE").ok(),
        requests_ca_bundle: std::env::var("REQUESTS_CA_BUNDLE").ok(),
        curl_ca_bundle: std::env::var("CURL_CA_BUNDLE").ok(),
        git_ssl_cainfo: std::env::var("GIT_SSL_CAINFO").ok(),
    }
}

fn check_proxy_environment_in(env: ProxyEnvironment, report: &mut DoctorReport) {
    let active_proxy = env
        .http
        .as_ref()
        .or(env.https.as_ref())
        .or(env.all.as_ref());
    match (&env.http, &env.https, &env.all) {
        (Some(http), Some(https), _) => {
            if http == https {
                report.warn(format!(
                    "Current calciforge doctor process has ambient HTTP(S)_PROXY configured ({}); if the service runs with the same env, model-provider, channel, and control-plane traffic can route through security-proxy. Prefer no ambient proxy on the Calciforge service.",
                    display_proxy_value(http)
                ));
            } else {
                report.warn(format!(
                    "Current calciforge doctor process has ambient HTTP_PROXY and HTTPS_PROXY configured but they differ; HTTP_PROXY={}, HTTPS_PROXY={}. Prefer no ambient proxy on the Calciforge service.",
                    display_proxy_value(http),
                    display_proxy_value(https)
                ));
            }
        }
        (Some(http), None, _) => report.warn(format!(
            "Current calciforge doctor process has ambient HTTP_PROXY set ({}). Prefer no ambient proxy on the Calciforge service.",
            display_proxy_value(http)
        )),
        (None, Some(https), _) => report.warn(format!(
            "Current calciforge doctor process has ambient HTTPS_PROXY set ({}) but HTTP_PROXY is not set. Prefer no ambient proxy on the Calciforge service.",
            display_proxy_value(https)
        )),
        (None, None, Some(all)) => report.warn(format!(
            "Current calciforge doctor process has ambient ALL_PROXY set ({}). Prefer no ambient proxy on the Calciforge service.",
            display_proxy_value(all)
        )),
        (None, None, None) => report
            .ok("Current calciforge doctor process has no ambient HTTP_PROXY/HTTPS_PROXY/ALL_PROXY"),
    }

    if active_proxy.is_some() {
        let no_proxy = env.no_proxy.unwrap_or_default();
        if no_proxy.contains("127.0.0.1") || no_proxy.contains("localhost") {
            report.ok("NO_PROXY includes local loopback");
        } else {
            report.warn("NO_PROXY does not include localhost/127.0.0.1; local health calls may loop through security-proxy");
        }
    }
}

#[cfg(test)]
fn check_agent_proxy_coverage(
    config: &CalciforgeConfig,
    env: &ProxyEnvironment,
    report: &mut DoctorReport,
) {
    let strict_egress = security_requires_agent_egress_proxy(config);
    check_agent_proxy_coverage_with_strict(config, env, strict_egress, report);
}

fn check_agent_proxy_coverage_with_strict(
    config: &CalciforgeConfig,
    env: &ProxyEnvironment,
    strict_egress: bool,
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
        let complete_count = subprocess_agents
            .iter()
            .filter(|agent| has_complete_agent_proxy_env(agent))
            .count();
        let clearing_count = subprocess_agents
            .iter()
            .filter(|agent| clears_agent_proxy_env(agent))
            .count();
        let incomplete_count = subprocess_agents
            .iter()
            .filter(|agent| has_incomplete_agent_proxy_env(agent))
            .count();

        if has_any_forward_proxy(env) {
            let message = "Current calciforge doctor process has ambient proxy env; subprocess inheritance works only if the service has the same env, and it can break CLI agents that use CONNECT, WebSockets, npm, or browser-backed auth. Prefer no ambient proxy and only wrap agents through tested recipes.";
            if strict_egress {
                report.error(message);
            } else {
                report.warn(message);
            }
        }

        if clearing_count > 0 {
            let message = format!(
                "{clearing_count} subprocess agent(s) set empty proxy env values; CLI/exec agents may bypass security-proxy"
            );
            if strict_egress {
                report.error(message);
            } else {
                report.warn(message);
            }
        }

        if incomplete_count > 0 {
            let message = format!(
                "{incomplete_count} subprocess agent(s) define incomplete MITM proxy env; require HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, loopback NO_PROXY, and at least one runtime CA bundle env"
            );
            if strict_egress {
                report.error(message);
            } else {
                report.warn(message);
            }
        }

        if complete_count > 0 {
            let message = format!(
                "{complete_count} subprocess agent(s) define complete MITM proxy env for tested runtime wrappers"
            );
            if strict_egress {
                report.ok(message);
            } else {
                report.warn(message);
            }
        }

        let missing_count = subprocess_agents
            .iter()
            .filter(|agent| {
                !has_complete_agent_proxy_env(agent)
                    && !has_incomplete_agent_proxy_env(agent)
                    && !clears_agent_proxy_env(agent)
            })
            .count();
        if missing_count > 0 {
            let message = format!(
                "{missing_count} subprocess agent(s) have no explicit MITM proxy env; use explicit tool/fetch integration or a tested wrapper for traffic that must pass through security-proxy"
            );
            if strict_egress {
                report.error(message);
            } else {
                report.ok(message);
            }
        }
    }

    if subprocess_count == 0 && has_any_forward_proxy(env) {
        report.warn(
            "Current calciforge doctor process has ambient proxy env but no subprocess agents need it; remove proxy env from the Calciforge service if present",
        );
    }

    if external_count > 0 {
        let message = format!(
            "{external_count} externally managed HTTP/native agent endpoint(s) configured; doctor cannot verify their process proxy environment"
        );
        if strict_egress {
            report.error(message);
        } else {
            report.warn(message);
        }
    }
}

fn security_requires_agent_egress_proxy(config: &CalciforgeConfig) -> bool {
    security_requires_agent_egress_proxy_with_override(config, None)
}

fn security_requires_agent_egress_proxy_with_override(
    config: &CalciforgeConfig,
    require_override: Option<&str>,
) -> bool {
    require_override.is_some_and(truthy_env_value)
        || config.security.as_ref().is_some_and(|security| {
            let profile_requires_egress = matches!(
                security.profile.as_str(),
                "hardened" | "maximum" | "paranoid"
            );
            let scans_agent_responses = security.scan_outbound.unwrap_or(profile_requires_egress);
            security.require_agent_egress_proxy || scans_agent_responses
        })
}

fn truthy_env_value(value: &str) -> bool {
    matches!(
        value.trim(),
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

fn check_model_gateway_config(config: &CalciforgeConfig, report: &mut DoctorReport) {
    let Some(proxy) = config.proxy.as_ref().filter(|proxy| proxy.enabled) else {
        report.ok("model gateway disabled");
        return;
    };

    let first_class_models = configured_first_class_model_ids(config);
    report.ok(format!(
        "model gateway selectors configured: {}",
        first_class_models.len()
    ));

    match routing::build_provider_entries(proxy, proxy.timeout_seconds) {
        Ok(entries) => {
            report.ok(format!(
                "model gateway provider routing loads: {} route entries",
                entries.len()
            ));
            report_model_gateway_provider_boundaries(proxy, report);
            check_model_gateway_route_graph(config, proxy, &entries, report);
        }
        Err(err) => report.error(format!("model gateway provider config invalid: {err}")),
    }
}

fn report_model_gateway_provider_boundaries(
    proxy: &crate::config::ProxyConfig,
    report: &mut DoctorReport,
) {
    for provider in &proxy.providers {
        if provider.backend_type != "http" {
            continue;
        }

        match provider.model_credential_owner {
            crate::config::CredentialOwner::Provider => report.ok(format!(
                "provider '{}' uses builtin HTTP transport to a provider-owned endpoint",
                provider.id
            )),
            crate::config::CredentialOwner::Calciforge => report.warn(format!(
                "provider '{}' uses Calciforge-owned builtin HTTP upstream credentials; this route is not handled by an external provider dashboard or registry",
                provider.id
            )),
        }
    }
}

fn check_model_gateway_route_graph(
    config: &CalciforgeConfig,
    proxy: &crate::config::ProxyConfig,
    entries: &[routing::ProviderEntry],
    report: &mut DoctorReport,
) {
    let alloy_manager = match AlloyManager::from_gateway_configs(
        &config.alloys,
        &config.cascades,
        &config.dispatchers,
    ) {
        Ok(manager) => manager,
        Err(err) => {
            report.error(format!(
                "model gateway synthetic route graph invalid: {err}"
            ));
            return;
        }
    };
    let effective_shortcuts = config.effective_model_shortcuts();
    let resolver = ModelResolver::new(&effective_shortcuts, &alloy_manager);
    let mut selectors: Vec<_> = gateway_model_selector_ids(config).into_iter().collect();
    selectors.sort();

    let mut explicit_routes = 0usize;
    let mut default_routes = 0usize;
    for selector in &selectors {
        let resolved = match resolver.plan_for_model(selector, 0) {
            Ok(resolved) => resolved,
            Err(err) => {
                report.error(format!(
                    "model gateway selector '{selector}' cannot resolve route graph: {err}"
                ));
                continue;
            }
        };

        for concrete_model in &resolved.plan.ordered_models {
            if routing::find_provider(entries, concrete_model).is_some() {
                explicit_routes += 1;
            } else {
                default_routes += 1;
                if !entries.is_empty()
                    && crate::proxy::backend_accepts_unlisted_models(&proxy.backend_type)
                {
                    report.warn(format!(
                        "model gateway selector '{selector}' resolves concrete model '{concrete_model}' through the default {} gateway, not an explicit provider route; add [[proxy.model_routes]] or a provider model pattern if it needs provider-specific API keys, prefixes, or on_switch hooks",
                        proxy.backend_type
                    ));
                }
            }
        }
    }

    report.ok(format!(
        "model gateway route graph resolves {} selector(s): {} explicit provider route(s), {} default gateway fallback route(s)",
        selectors.len(),
        explicit_routes,
        default_routes
    ));
}

fn report_agent_protection_summary(config: &CalciforgeConfig, report: &mut DoctorReport) {
    let proxy = config.proxy.as_ref().filter(|proxy| proxy.enabled);
    let proxy_bind = proxy.map(|proxy| proxy.bind.as_str());
    let gateway_engine = proxy
        .map(|proxy| proxy.backend_type.as_str())
        .unwrap_or("disabled");

    for agent in &config.agents {
        let model_gateway = agent_model_gateway_coverage(agent, proxy_bind, gateway_engine);
        let model_override = if agent_supports_model_override(agent) {
            "enabled"
        } else if agent.allow_model_override == Some(false) {
            "disabled explicitly"
        } else {
            "disabled"
        };
        let security_proxy = agent_security_proxy_coverage(agent, proxy_bind);

        report.ok(format!(
            "agent '{}' coverage: model_gateway={}, model_override={}, security_proxy={}",
            agent.id, model_gateway, model_override, security_proxy
        ));
    }
}

fn agent_model_gateway_coverage(
    agent: &AgentConfig,
    proxy_bind: Option<&str>,
    gateway_engine: &str,
) -> String {
    match parse_agent_kind(&agent.kind) {
        Some(AgentKind::OpenAiCompat)
            if proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind)) =>
        {
            format!("yes via Calciforge proxy ({gateway_engine})")
        }
        Some(AgentKind::OpenAiCompat) => {
            "no; openai-compat points at an external model endpoint".to_string()
        }
        Some(kind) if kind.is_subprocess_agent() => {
            "no; subprocess agent manages its own model/provider calls".to_string()
        }
        Some(kind) if kind.is_http_agent() => {
            "no; downstream HTTP agent manages its own model/provider calls".to_string()
        }
        Some(_) => "no; adapter does not use the model gateway".to_string(),
        None => "unknown; unrecognized adapter kind".to_string(),
    }
}

fn agent_security_proxy_coverage(agent: &AgentConfig, proxy_bind: Option<&str>) -> &'static str {
    match parse_agent_kind(&agent.kind) {
        Some(AgentKind::OpenAiCompat)
            if proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind)) =>
        {
            "Calciforge model-boundary path; not ambient MITM"
        }
        Some(kind) if kind.is_subprocess_agent() => {
            if has_complete_agent_proxy_env(agent) {
                "explicit proxy env configured; verify this runtime honors it"
            } else if has_incomplete_agent_proxy_env(agent) {
                "partial proxy env configured"
            } else if clears_agent_proxy_env(agent) {
                "explicitly clears proxy env"
            } else {
                "not configured for subprocess"
            }
        }
        Some(kind) if kind.is_http_agent() => {
            "unknown; downstream daemon process is outside Calciforge"
        }
        Some(_) => "not applicable",
        None => "unknown",
    }
}

fn agent_proxy_environment(agent: &AgentConfig) -> ProxyEnvironment {
    let env = agent.env.as_ref();
    ProxyEnvironment {
        http: env.and_then(|env| {
            env.get("HTTP_PROXY")
                .or_else(|| env.get("http_proxy"))
                .cloned()
        }),
        https: env.and_then(|env| {
            env.get("HTTPS_PROXY")
                .or_else(|| env.get("https_proxy"))
                .cloned()
        }),
        all: env.and_then(|env| {
            env.get("ALL_PROXY")
                .or_else(|| env.get("all_proxy"))
                .cloned()
        }),
        no_proxy: env.and_then(|env| env.get("NO_PROXY").or_else(|| env.get("no_proxy")).cloned()),
        node_extra_ca_certs: env.and_then(|env| env.get("NODE_EXTRA_CA_CERTS").cloned()),
        ssl_cert_file: env.and_then(|env| env.get("SSL_CERT_FILE").cloned()),
        requests_ca_bundle: env.and_then(|env| env.get("REQUESTS_CA_BUNDLE").cloned()),
        curl_ca_bundle: env.and_then(|env| env.get("CURL_CA_BUNDLE").cloned()),
        git_ssl_cainfo: env.and_then(|env| env.get("GIT_SSL_CAINFO").cloned()),
    }
}

fn has_complete_agent_proxy_env(agent: &AgentConfig) -> bool {
    has_complete_mitm_proxy_env(&agent_proxy_environment(agent))
}

fn has_incomplete_agent_proxy_env(agent: &AgentConfig) -> bool {
    has_any_agent_proxy_env(agent)
        && !has_complete_agent_proxy_env(agent)
        && !clears_agent_proxy_env(agent)
}

fn has_any_agent_proxy_env(agent: &AgentConfig) -> bool {
    agent
        .env
        .as_ref()
        .is_some_and(|env| env.keys().any(|key| is_proxy_env_key(key)))
}

fn has_http_proxy(env: &ProxyEnvironment) -> bool {
    env.http
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn has_any_forward_proxy(env: &ProxyEnvironment) -> bool {
    [&env.http, &env.https, &env.all].into_iter().any(|value| {
        value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn has_complete_mitm_proxy_env(env: &ProxyEnvironment) -> bool {
    has_http_proxy(env)
        && env
            .https
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && env
            .all
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && has_runtime_ca_bundle_env(env)
        && env
            .no_proxy
            .as_deref()
            .is_some_and(no_proxy_includes_loopback)
}

fn has_runtime_ca_bundle_env(env: &ProxyEnvironment) -> bool {
    [
        &env.node_extra_ca_certs,
        &env.ssl_cert_file,
        &env.requests_ca_bundle,
        &env.curl_ca_bundle,
        &env.git_ssl_cainfo,
    ]
    .into_iter()
    .any(|value| {
        value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn no_proxy_includes_loopback(value: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|entry| matches!(entry, "localhost" | "127.0.0.1" | "::1"))
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
    find_executable_for_agent(bin, None)
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
        if agent.reply_auth_token.is_some() {
            report.warn(format!(
                "agent '{}' stores an inline reply_auth_token; prefer reply_auth_token_file",
                agent.id
            ));
        }
        if let Some(path) = &agent.reply_auth_token_file {
            check_readable_file(
                report,
                "agent reply_auth_token_file",
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

async fn check_scanner_config(
    config: &CalciforgeConfig,
    no_network: bool,
    report: &mut DoctorReport,
) {
    let Some(security) = &config.security else {
        report.ok("security scanner uses profile default built-in Starlark policy");
        return;
    };

    if security.scanner_checks.is_empty() {
        report.ok(format!(
            "security scanner profile '{}' uses default built-in Starlark policy",
            security.profile
        ));
        return;
    }

    report.ok(format!(
        "security scanner profile '{}' has {} configured check(s)",
        security.profile,
        security.scanner_checks.len()
    ));

    for (idx, check) in security.scanner_checks.iter().enumerate() {
        match check {
            ScannerCheckConfig::RemoteHttp { url, fail_closed } => {
                check_remote_scanner_url(idx, url, *fail_closed, no_network, report);
            }
            ScannerCheckConfig::Starlark {
                path,
                fail_closed,
                max_callstack,
            } => {
                check_starlark_scanner_policy(idx, path, *fail_closed, *max_callstack, report)
                    .await;
            }
        }
    }
}

fn check_remote_scanner_url(
    idx: usize,
    url: &str,
    fail_closed: bool,
    no_network: bool,
    report: &mut DoctorReport,
) {
    let parsed = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(err) => {
            report.error(format!(
                "scanner check #{idx} remote_http URL is invalid: {err}"
            ));
            return;
        }
    };

    if !matches!(parsed.scheme(), "http" | "https") {
        report.error(format!(
            "scanner check #{idx} remote_http URL must use http or https"
        ));
        return;
    }

    if parsed.host_str().is_none() {
        report.error(format!("scanner check #{idx} remote_http URL has no host"));
        return;
    }

    if no_network {
        report.ok(format!(
            "scanner check #{idx} remote_http URL parses; reachability skipped by --no-network"
        ));
    } else {
        report.ok(format!(
            "scanner check #{idx} remote_http URL parses; fail_closed={fail_closed}"
        ));
    }
}

async fn check_starlark_scanner_policy(
    idx: usize,
    path: &str,
    fail_closed: bool,
    max_callstack: usize,
    report: &mut DoctorReport,
) {
    let expanded = config::expand_tilde(path);
    match std::fs::metadata(&expanded) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            report.error(format!(
                "scanner check #{idx} starlark policy is not a regular file"
            ));
            return;
        }
        Err(err) => {
            report.error(format!(
                "scanner check #{idx} starlark policy is not readable: {err}"
            ));
            return;
        }
    }

    let scanner = AdversaryScanner::new(ScannerConfig {
        checks: vec![ScannerCheckConfig::Starlark {
            path: expanded.to_string_lossy().into_owned(),
            fail_closed: true,
            max_callstack,
        }],
        ..Default::default()
    });
    let verdict = scanner
        .scan(
            "https://calciforge.local/doctor",
            "calciforge doctor scanner policy validation probe",
            ScanContext::Api,
        )
        .await;

    match verdict {
        ScanVerdict::Unsafe { reason }
            if reason.contains("starlark security check failed")
                || reason.contains("policy must define scan(input)") =>
        {
            report.error(format!(
                "scanner check #{idx} starlark policy failed validation: {reason}"
            ));
        }
        ScanVerdict::Unsafe { reason } => {
            report.warn(format!(
                "scanner check #{idx} starlark policy loaded, but blocks the doctor probe: {reason}"
            ));
        }
        ScanVerdict::Review { reason } => {
            report.warn(format!(
                "scanner check #{idx} starlark policy loaded, but reviews the doctor probe: {reason}"
            ));
        }
        ScanVerdict::Clean => {
            report.ok(format!(
                "scanner check #{idx} starlark policy loads; configured fail_closed={fail_closed}"
            ));
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

fn check_agent_runtime_dependencies(agent: &AgentConfig, report: &mut DoctorReport) {
    let Some(kind) = parse_agent_kind(&agent.kind) else {
        return;
    };
    if !kind.is_subprocess_agent() {
        return;
    }

    if kind == AgentKind::Acpx {
        match find_executable_for_agent("acpx", agent.env.as_ref()) {
            Some(path) => report.ok(format!(
                "agent '{}' acpx runtime found at {}",
                agent.id,
                path.display()
            )),
            None => report.error(format!(
                "agent '{}' kind 'acpx' requires acpx on the effective PATH used for this agent; when env.PATH is configured it replaces Calciforge's service PATH",
                agent.id
            )),
        }
    }

    match subprocess_command_for_agent(agent) {
        Some(command) => match find_executable_for_agent(command, agent.env.as_ref()) {
            Some(path) => report.ok(format!(
                "agent '{}' subprocess command '{}' found at {}",
                agent.id,
                command,
                path.display()
            )),
            None => report.error(format!(
                "agent '{}' kind '{}' command '{}' is not on the effective PATH used for this agent; when env.PATH is configured it replaces Calciforge's service PATH",
                agent.id, agent.kind, command
            )),
        },
        None => report.error(format!(
            "agent '{}' kind '{}' requires command",
            agent.id, agent.kind
        )),
    }
}

#[cfg(test)]
async fn check_agent_wiring(
    config: &CalciforgeConfig,
    no_network: bool,
    report: &mut DoctorReport,
) {
    check_agent_wiring_with_strict(
        config,
        no_network,
        security_requires_agent_egress_proxy(config),
        report,
    )
    .await;
}

async fn check_agent_wiring_with_strict(
    config: &CalciforgeConfig,
    no_network: bool,
    strict_egress_proxy: bool,
    report: &mut DoctorReport,
) {
    let proxy_bind = config.proxy.as_ref().map(|proxy| proxy.bind.as_str());
    let mut endpoint_counts: HashMap<&str, usize> = HashMap::new();

    for agent in &config.agents {
        if agent.kind == "openclaw-http" {
            report.error(format!(
                "agent '{}' uses removed kind 'openclaw-http'; migrate to kind='openclaw-channel' and install the Calciforge OpenClaw channel plugin",
                agent.id
            ));
            continue;
        }

        if agent.kind == "openclaw-native" {
            report.error(format!(
                "agent '{}' uses unsupported kind 'openclaw-native'; /hooks/agent is async automation, not a synchronous chat adapter. Use kind='openclaw-channel'",
                agent.id
            ));
            continue;
        }

        match agent_kind_metadata(&agent.kind) {
            Some(metadata) => match metadata.lifecycle {
                AgentKindLifecycle::Stable => {}
                AgentKindLifecycle::Legacy => report.warn(format!(
                    "agent '{}' uses {} kind '{}': {}",
                    agent.id,
                    metadata.lifecycle.label(),
                    metadata.name,
                    metadata.summary
                )),
                AgentKindLifecycle::Experimental => report.warn(format!(
                    "agent '{}' uses {} kind '{}': {}",
                    agent.id,
                    metadata.lifecycle.label(),
                    metadata.name,
                    metadata.summary
                )),
            },
            None => {
                report.error(format!(
                    "agent '{}' has unknown kind '{}'; known kinds: {}",
                    agent.id,
                    agent.kind,
                    known_agent_kind_names().collect::<Vec<_>>().join(", ")
                ));
            }
        }

        check_agent_runtime_dependencies(agent, report);

        if is_http_agent(agent) {
            if agent.endpoint.trim().is_empty() {
                report.error(format!(
                    "agent '{}' kind '{}' requires endpoint",
                    agent.id, agent.kind
                ));
                continue;
            }
            *endpoint_counts.entry(agent.endpoint.as_str()).or_default() += 1;

            if proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind))
                && agent.id != "gateway"
            {
                report.warn(format!(
                    "agent '{}' points at the local Calciforge proxy; use a clearly named raw gateway agent or route to the real downstream agent",
                    agent.id
                ));
            }

            if agent.kind == "openclaw-channel" {
                if agent.reply_auth_token.is_none() && agent.reply_auth_token_file.is_none() {
                    report.warn(format!(
                        "agent '{}' uses openclaw-channel without reply_auth_token/reply_auth_token_file; callback replies should be bearer-protected outside isolated local tests",
                        agent.id
                    ));
                }

                if agent.api_key.is_none()
                    && agent.api_key_file.is_none()
                    && agent.auth_token.is_none()
                {
                    report.warn(format!(
                        "agent '{}' uses openclaw-channel without api_key/api_key_file/auth_token; no per-agent token is configured, though adapters may still fall back to CALCIFORGE_AGENT_TOKEN. Only loopback gateways intended to rely on that setup should do this",
                        agent.id
                    ));
                }
            }

            if agent.kind == "openai-compat"
                && agent.model.is_none()
                && agent.allow_model_override != Some(true)
            {
                report.error(format!(
                    "agent '{}' uses openai-compat without a configured model; set model or allow_model_override = true to forward !model overrides",
                    agent.id
                ));
            }

            if agent.kind == "openai-compat"
                && agent.model.as_deref().is_some_and(is_openclaw_model_id)
            {
                report.error(format!(
                    "agent '{}' uses openai-compat with OpenClaw model '{}'; OpenClaw agent chat must use kind='openclaw-channel'",
                    agent.id,
                    agent.model.as_deref().unwrap_or_default()
                ));
            }

            if !no_network {
                check_endpoint_reachable(agent, report).await;
                if agent.kind == "openclaw-channel" {
                    check_openclaw_channel_route(agent, strict_egress_proxy, report).await;
                }
            }
        }

        agent_adapter_doctor::check(agent, config, no_network, report).await;
    }

    for (endpoint, count) in endpoint_counts {
        if count > 1 {
            report.warn(format!(
                "{count} agents share endpoint {endpoint}; verify these are distinct lanes rather than stale copy/paste"
            ));
        }
    }
}

fn is_http_agent(agent: &AgentConfig) -> bool {
    parse_agent_kind(&agent.kind).is_some_and(AgentKind::is_http_agent)
}

fn is_subprocess_agent(agent: &AgentConfig) -> bool {
    parse_agent_kind(&agent.kind).is_some_and(AgentKind::is_subprocess_agent)
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
        "HTTP_PROXY"
            | "http_proxy"
            | "HTTPS_PROXY"
            | "https_proxy"
            | "ALL_PROXY"
            | "all_proxy"
            | "NO_PROXY"
            | "no_proxy"
            | "NODE_EXTRA_CA_CERTS"
            | "SSL_CERT_FILE"
            | "REQUESTS_CA_BUNDLE"
            | "CURL_CA_BUNDLE"
            | "GIT_SSL_CAINFO"
    )
}

fn is_external_agent_daemon(agent: &AgentConfig, proxy_bind: Option<&str>) -> bool {
    is_http_agent(agent)
        && !proxy_bind.is_some_and(|bind| endpoint_matches_bind(&agent.endpoint, bind))
}

fn is_openclaw_model_id(model: &str) -> bool {
    let trimmed = model.trim();
    trimmed == "openclaw" || trimmed.starts_with("openclaw/")
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

async fn check_openclaw_channel_route(
    agent: &AgentConfig,
    strict_egress: bool,
    report: &mut DoctorReport,
) {
    let route = format!(
        "{}/calciforge/inbound",
        agent.endpoint.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            report.error(format!(
                "agent '{}' openclaw-channel route check could not build HTTP client: {err}",
                agent.id
            ));
            return;
        }
    };

    let inbound_token = resolve_agent_inbound_token(agent, true);
    let supplied_inbound_token = inbound_token.is_some();
    let mut request = client.get(&route);
    if let Some(token) = inbound_token.as_deref() {
        request = request.bearer_auth(token);
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                let message = if supplied_inbound_token {
                    format!(
                        "agent '{}' /calciforge/inbound rejected the configured inbound token ({status}); verify api_key_file/api_key/auth_token/CALCIFORGE_AGENT_TOKEN matches the OpenClaw channel plugin token",
                        agent.id
                    )
                } else {
                    format!(
                        "agent '{}' /calciforge/inbound requires auth ({status}), but doctor could not resolve an inbound token; configure api_key_file/api_key/auth_token or CALCIFORGE_AGENT_TOKEN",
                        agent.id
                    )
                };
                if supplied_inbound_token {
                    report.error(message);
                } else {
                    report.warn(message);
                }
            } else if openclaw_channel_route_status_is_present(status) {
                report.ok(format!(
                    "agent '{}' exposes openclaw-channel route at /calciforge/inbound ({status})",
                    agent.id
                ));
                if status == reqwest::StatusCode::OK {
                    match response.json::<OpenClawChannelStatus>().await {
                        Ok(status) => {
                            let source_ip =
                                local_source_ip_for_endpoint_with_timeout(&agent.endpoint).await;
                            check_openclaw_channel_status(
                                agent,
                                &status,
                                source_ip,
                                strict_egress,
                                report,
                            );
                        }
                        Err(err) => report.warn(format!(
                            "agent '{}' openclaw-channel status response was not recognized: {err}",
                            agent.id
                        )),
                    }
                }
            } else if status == reqwest::StatusCode::NOT_FOUND {
                report.error(format!(
                    "agent '{}' endpoint is reachable but /calciforge/inbound returns 404; install or enable the Calciforge OpenClaw channel plugin",
                    agent.id
                ));
            } else if status.is_server_error() {
                report.error(format!(
                    "agent '{}' /calciforge/inbound returned server error {status}",
                    agent.id
                ));
            } else {
                report.warn(format!(
                    "agent '{}' /calciforge/inbound returned unexpected status {status}; verify the Calciforge OpenClaw channel plugin is installed",
                    agent.id
                ));
            }
        }
        Err(err) => {
            report.error(format!(
                "agent '{}' /calciforge/inbound route check failed: {err}",
                agent.id
            ));
        }
    }
}

fn openclaw_channel_route_status_is_present(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::OK
            | reqwest::StatusCode::ACCEPTED
            | reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawChannelStatus {
    plugin: Option<String>,
    reply_webhook: Option<String>,
    reply_auth_token_sha256: Option<String>,
    egress_proxy: Option<OpenClawEgressProxyStatus>,
    model_runtime: Option<OpenClawModelRuntimeStatus>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawEgressProxyStatus {
    http_proxy: bool,
    https_proxy: bool,
    all_proxy: bool,
    node_extra_ca_certs: bool,
    ssl_cert_file: bool,
    requests_ca_bundle: bool,
    curl_ca_bundle: bool,
    git_ssl_ca_info: bool,
    no_proxy_loopback: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawModelRuntimeStatus {
    ok: bool,
    agent_runtime: Option<String>,
    primary: Option<String>,
    fallbacks: Vec<String>,
    unsupported: Vec<OpenClawUnsupportedModelProvider>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawUnsupportedModelProvider {
    model: Option<String>,
    provider: Option<String>,
    reason: String,
}

fn check_openclaw_channel_status(
    agent: &AgentConfig,
    status: &OpenClawChannelStatus,
    source_ip: Option<IpAddr>,
    strict_egress: bool,
    report: &mut DoctorReport,
) {
    if status.plugin.as_deref() != Some("calciforge-channel") {
        report.warn(format!(
            "agent '{}' /calciforge/inbound status did not identify the Calciforge channel plugin",
            agent.id
        ));
    }

    if let Some(actual_hash) = status.reply_auth_token_sha256.as_deref() {
        match resolve_agent_reply_token(agent) {
            Ok(Some(expected_token)) => {
                let expected_hash = sha256_prefix(&expected_token);
                if actual_hash == expected_hash {
                    report.ok(format!(
                        "agent '{}' openclaw-channel reply token hash matches Calciforge config",
                        agent.id
                    ));
                } else {
                    report.error(format!(
                        "agent '{}' openclaw-channel reply token hash does not match Calciforge reply_auth_token/reply_auth_token_file; callbacks will be rejected",
                        agent.id
                    ));
                }
            }
            Ok(None) => {}
            Err(err) => report.error(err),
        }
    }

    check_openclaw_egress_proxy_status(agent, status.egress_proxy.as_ref(), strict_egress, report);
    check_openclaw_model_runtime_status(agent, status.model_runtime.as_ref(), report);

    let Some(reply_webhook) = status.reply_webhook.as_deref() else {
        return;
    };
    let Ok(reply_url) = reqwest::Url::parse(reply_webhook) else {
        report.warn(format!(
            "agent '{}' openclaw-channel reported invalid replyWebhook URL",
            agent.id
        ));
        return;
    };

    let expected_port = agent.reply_port.unwrap_or(18797);
    if reply_url.port_or_known_default() != Some(expected_port) {
        report.error(format!(
            "agent '{}' openclaw-channel replyWebhook uses port {}, but Calciforge expects callback port {}; callbacks will miss the reply listener",
            agent.id,
            reply_url
                .port_or_known_default()
                .map(|port| port.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            expected_port
        ));
    }

    let Some(source_ip) = source_ip else {
        return;
    };
    let Some(reply_host) = reply_url.host_str() else {
        return;
    };
    let Ok(reply_ip) = reply_host.parse::<IpAddr>() else {
        return;
    };
    if !is_loopback_ip(reply_ip) && reply_ip != source_ip {
        report.warn(format!(
            "agent '{}' openclaw-channel replyWebhook host {} does not match this host's source address {} for the agent endpoint; this often means a stale callback URL from another Calciforge install",
            agent.id, reply_ip, source_ip
        ));
    }
}

fn check_openclaw_model_runtime_status(
    agent: &AgentConfig,
    status: Option<&OpenClawModelRuntimeStatus>,
    report: &mut DoctorReport,
) {
    let Some(status) = status else {
        report.warn(format!(
            "agent '{}' openclaw-channel did not report model runtime compatibility; upgrade the Calciforge OpenClaw channel plugin before relying on deployment preflight",
            agent.id
        ));
        return;
    };

    if status.ok {
        let primary = status.primary.as_deref().unwrap_or("(none)");
        report.ok(format!(
            "agent '{}' openclaw-channel model route is compatible with runtime '{}': primary={}, fallbacks={}",
            agent.id,
            status.agent_runtime.as_deref().unwrap_or("(unknown)"),
            primary,
            status.fallbacks.len()
        ));
        return;
    }

    let details = status
        .unsupported
        .iter()
        .map(|entry| {
            let model = entry.model.as_deref().unwrap_or("(unknown model)");
            let provider = entry.provider.as_deref().unwrap_or("(unknown provider)");
            format!("{model} via {provider}: {}", entry.reason)
        })
        .collect::<Vec<_>>()
        .join("; ");
    report.error(format!(
        "agent '{}' openclaw-channel model route is incompatible with runtime '{}': {}; fix agents.defaults.model or agents.defaults.agentRuntime before deployment",
        agent.id,
        status.agent_runtime.as_deref().unwrap_or("(unknown)"),
        if details.is_empty() {
            "no compatible configured model route reported".to_string()
        } else {
            details
        }
    ));
}

fn check_openclaw_egress_proxy_status(
    agent: &AgentConfig,
    status: Option<&OpenClawEgressProxyStatus>,
    strict_egress: bool,
    report: &mut DoctorReport,
) {
    let Some(status) = status else {
        let message = format!(
            "agent '{}' openclaw-channel did not report runtime egress proxy status; upgrade the Calciforge OpenClaw channel plugin before relying on security-gateway enforcement",
            agent.id
        );
        if strict_egress {
            report.error(message);
        } else {
            report.warn(message);
        }
        return;
    };

    let has_ca_bundle = status.node_extra_ca_certs
        || status.ssl_cert_file
        || status.requests_ca_bundle
        || status.curl_ca_bundle
        || status.git_ssl_ca_info;
    let complete = status.http_proxy
        && status.https_proxy
        && status.all_proxy
        && status.no_proxy_loopback
        && has_ca_bundle;

    if complete {
        report.ok(format!(
            "agent '{}' openclaw-channel reports complete MITM proxy/CA egress env",
            agent.id
        ));
        return;
    }

    let message = format!(
        "agent '{}' openclaw-channel reports incomplete MITM proxy/CA egress env: HTTP_PROXY={}, HTTPS_PROXY={}, ALL_PROXY={}, CA bundle={}, loopback NO_PROXY={}",
        agent.id,
        status.http_proxy,
        status.https_proxy,
        status.all_proxy,
        has_ca_bundle,
        status.no_proxy_loopback
    );
    if strict_egress {
        report.error(message);
    } else {
        report.warn(message);
    }
}

async fn local_source_ip_for_endpoint(endpoint: &str) -> Option<IpAddr> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect(format!("{host}:{port}")).await.ok()?;
    Some(socket.local_addr().ok()?.ip())
}

async fn local_source_ip_for_endpoint_with_timeout(endpoint: &str) -> Option<IpAddr> {
    timeout(
        Duration::from_millis(250),
        local_source_ip_for_endpoint(endpoint),
    )
    .await
    .ok()
    .flatten()
}

fn resolve_agent_inbound_token(agent: &AgentConfig, allow_env: bool) -> Option<String> {
    if let Some(path) = &agent.api_key_file {
        let path = config::expand_tilde(&path.to_string_lossy());
        let token = std::fs::read_to_string(path).ok()?.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    if let Some(token) = &agent.api_key {
        return Some(token.clone());
    }
    if let Some(token) = &agent.auth_token {
        return Some(token.clone());
    }
    if allow_env {
        return std::env::var("CALCIFORGE_AGENT_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty());
    }
    None
}

fn resolve_agent_reply_token(agent: &AgentConfig) -> std::result::Result<Option<String>, String> {
    if let Some(path) = &agent.reply_auth_token_file {
        let path = config::expand_tilde(&path.to_string_lossy());
        let token = std::fs::read_to_string(path).map_err(|err| {
            format!(
                "agent '{}': failed to read reply_auth_token_file for doctor route check: {err}",
                agent.id
            )
        })?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(format!(
                "agent '{}': reply_auth_token_file is empty",
                agent.id
            ));
        }
        return Ok(Some(token));
    }
    Ok(agent
        .reply_auth_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned))
}

fn sha256_prefix(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(digest)[..16].to_string()
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
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
    let gateway_model_selectors = gateway_model_selector_ids(config);

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
            if gateway_model_selectors.contains(&model_id) {
                report.ok(format!(
                    "active model override for '{identity}' points to '{model_id}'"
                ));
            } else {
                report.error(format!(
                    "active model override for '{identity}' points to unknown gateway model selector '{model_id}'"
                ));
            }
        }
    }
}

fn default_state_dir() -> PathBuf {
    crate::config::calciforge_config_home(None).join("state")
}

fn read_state_map(path: &Path) -> Result<HashMap<String, String>, ()> {
    let text = std::fs::read_to_string(path).map_err(|_| ())?;
    serde_json::from_str(&text).map_err(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    use crate::config::{
        CalciforgeHeader, ModelRoleConfig, ProxyConfig, ProxyModelRoute, ProxyProviderConfig,
        RoutingRule, SecuritySectionConfig, SyntheticModelConfig,
    };

    fn base_config() -> CalciforgeConfig {
        CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![],
            agents: vec![
                AgentConfig {
                    id: "gateway".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://127.0.0.1:18083".to_string(),
                    model: Some("local-kimi-gpt55".to_string()),
                    api_key_file: Some(PathBuf::from("/tmp/nonexistent-test-token")),
                    ..Default::default()
                },
                AgentConfig {
                    id: "custodian".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://127.0.0.1:18083".to_string(),
                    model: Some("local-kimi-gpt55".to_string()),
                    api_key_file: Some(PathBuf::from("/tmp/nonexistent-test-token")),
                    ..Default::default()
                },
            ],
            routing: vec![RoutingRule {
                identity: "brian".to_string(),
                default_agent: "gateway".to_string(),
                btw_agent: None,
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
            model_roles: vec![],
            alloys: vec![],
            cascades: vec![],
            exec_models: vec![],
            security: None,
            local_models: None,
        }
    }

    #[test]
    fn openclaw_channel_status_detects_reply_token_mismatch() {
        let expected_reply = ["expected", "reply"].join("-");
        let agent = AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://198.51.100.20:18790".to_string(),
            reply_auth_token: Some(expected_reply),
            reply_port: Some(18797),
            ..Default::default()
        };
        let status = OpenClawChannelStatus {
            plugin: Some("calciforge-channel".to_string()),
            reply_webhook: Some("http://198.51.100.10:18797/hooks/reply".to_string()),
            reply_auth_token_sha256: Some(sha256_prefix(&["stale", "reply"].join("-"))),
            egress_proxy: None,
            model_runtime: None,
        };
        let mut report = DoctorReport::default();

        check_openclaw_channel_status(
            &agent,
            &status,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10))),
            false,
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("reply token hash does not match")
        }));
    }

    #[test]
    fn openclaw_channel_status_warns_on_stale_callback_host() {
        let expected_reply = ["expected", "reply"].join("-");
        let agent = AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://198.51.100.20:18790".to_string(),
            reply_auth_token: Some(expected_reply.clone()),
            reply_port: Some(18797),
            ..Default::default()
        };
        let status = OpenClawChannelStatus {
            plugin: Some("calciforge-channel".to_string()),
            reply_webhook: Some("http://198.51.100.30:18797/hooks/reply".to_string()),
            reply_auth_token_sha256: Some(sha256_prefix(&expected_reply)),
            egress_proxy: None,
            model_runtime: None,
        };
        let mut report = DoctorReport::default();

        check_openclaw_channel_status(
            &agent,
            &status,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10))),
            false,
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn && finding.message.contains("stale callback URL")
        }));
    }

    #[test]
    fn openclaw_channel_status_errors_when_strict_egress_status_missing() {
        let agent = AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://198.51.100.20:18790".to_string(),
            reply_port: Some(18797),
            ..Default::default()
        };
        let status = OpenClawChannelStatus {
            plugin: Some("calciforge-channel".to_string()),
            reply_webhook: Some("http://198.51.100.10:18797/hooks/reply".to_string()),
            reply_auth_token_sha256: None,
            egress_proxy: None,
            model_runtime: None,
        };
        let mut report = DoctorReport::default();

        check_openclaw_channel_status(
            &agent,
            &status,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10))),
            true,
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("did not report runtime egress proxy status")
        }));
    }

    #[test]
    fn openclaw_channel_status_accepts_complete_egress_status() {
        let agent = AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://198.51.100.20:18790".to_string(),
            reply_port: Some(18797),
            ..Default::default()
        };
        let status = OpenClawChannelStatus {
            plugin: Some("calciforge-channel".to_string()),
            reply_webhook: Some("http://198.51.100.10:18797/hooks/reply".to_string()),
            reply_auth_token_sha256: None,
            egress_proxy: Some(OpenClawEgressProxyStatus {
                http_proxy: true,
                https_proxy: true,
                all_proxy: true,
                node_extra_ca_certs: true,
                ssl_cert_file: false,
                requests_ca_bundle: false,
                curl_ca_bundle: false,
                git_ssl_ca_info: false,
                no_proxy_loopback: true,
            }),
            model_runtime: Some(OpenClawModelRuntimeStatus {
                ok: true,
                agent_runtime: Some("pi".to_string()),
                primary: Some("calciforge/gpt55-kimi26".to_string()),
                fallbacks: vec![],
                unsupported: vec![],
            }),
        };
        let mut report = DoctorReport::default();

        check_openclaw_channel_status(
            &agent,
            &status,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10))),
            true,
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding
                    .message
                    .contains("reports complete MITM proxy/CA egress env")
        }));
    }

    #[test]
    fn openclaw_channel_status_errors_on_incompatible_model_runtime() {
        let agent = AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://198.51.100.20:18790".to_string(),
            reply_port: Some(18797),
            ..Default::default()
        };
        let status = OpenClawChannelStatus {
            plugin: Some("calciforge-channel".to_string()),
            reply_webhook: Some("http://198.51.100.10:18797/hooks/reply".to_string()),
            reply_auth_token_sha256: None,
            egress_proxy: None,
            model_runtime: Some(OpenClawModelRuntimeStatus {
                ok: false,
                agent_runtime: Some("codex".to_string()),
                primary: Some("openai/gpt-5.5".to_string()),
                fallbacks: vec!["calciforge/gpt55-kimi26".to_string()],
                unsupported: vec![OpenClawUnsupportedModelProvider {
                    model: Some("calciforge/gpt55-kimi26".to_string()),
                    provider: Some("calciforge".to_string()),
                    reason:
                        "agentRuntime 'codex' cannot load configured model provider 'calciforge'"
                            .to_string(),
                }],
            }),
        };
        let mut report = DoctorReport::default();

        check_openclaw_channel_status(
            &agent,
            &status,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10))),
            false,
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("model route is incompatible")
                && finding.message.contains("calciforge/gpt55-kimi26")
        }));
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
    fn rejects_removed_openclaw_http_agent_kind() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "custodian".to_string(),
            kind: "openclaw-http".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("openclaw-http")
                && finding.message.contains("openclaw-channel")
        }));
    }

    #[test]
    fn warns_on_legacy_and_experimental_agent_kinds() {
        let mut config = base_config();
        config.agents = vec![
            AgentConfig {
                id: "legacy-agent".to_string(),
                kind: "zeroclaw".to_string(),
                endpoint: "http://127.0.0.1:18084".to_string(),
                api_key: Some("test-token".to_string()),
                ..Default::default()
            },
            AgentConfig {
                id: "experimental-agent".to_string(),
                kind: "acp".to_string(),
                command: Some("test-agent".to_string()),
                ..Default::default()
            },
        ];
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
                    .contains("agent 'legacy-agent' uses legacy kind 'zeroclaw'")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding
                    .message
                    .contains("agent 'experimental-agent' uses experimental kind 'acp'")
        }));
    }

    #[test]
    fn rejects_openai_compat_without_model_or_override_opt_in() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "gateway".to_string(),
            kind: "openai-compat".to_string(),
            endpoint: "http://127.0.0.1:8083".to_string(),
            api_key: Some("test-token".to_string()),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("openai-compat")
                && finding.message.contains("allow_model_override")
        }));
    }

    #[test]
    fn rejects_openai_compat_openclaw_model_ids() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "librarian".to_string(),
            kind: "openai-compat".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            api_key: Some("test-token".to_string()),
            model: Some("openclaw/main".to_string()),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("OpenClaw")
                && finding.message.contains("openclaw-channel")
        }));
    }

    #[test]
    fn agent_protection_summary_marks_local_gateway_agents() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "gateway".to_string(),
            kind: "openai-compat".to_string(),
            endpoint: "http://127.0.0.1:18083".to_string(),
            api_key: Some("test-token".to_string()),
            model: Some("balanced".to_string()),
            allow_model_override: Some(true),
            ..Default::default()
        }];
        config.proxy = Some(ProxyConfig {
            enabled: true,
            bind: "0.0.0.0:18083".to_string(),
            backend_type: "helicone".to_string(),
            ..Default::default()
        });
        let mut report = DoctorReport::default();

        report_agent_protection_summary(&config, &mut report);

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding.message.contains("agent 'gateway' coverage")
                && finding
                    .message
                    .contains("model_gateway=yes via Calciforge proxy (helicone)")
                && finding.message.contains("model_override=enabled")
        }));
    }

    #[test]
    fn agent_protection_summary_marks_subprocess_bypass() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "opencode".to_string(),
            kind: "acpx".to_string(),
            command: Some("opencode".to_string()),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        report_agent_protection_summary(&config, &mut report);

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding.message.contains("agent 'opencode' coverage")
                && finding.message.contains(
                    "model_gateway=no; subprocess agent manages its own model/provider calls",
                )
                && finding
                    .message
                    .contains("security_proxy=not configured for subprocess")
        }));
    }

    #[test]
    fn check_agent_wiring_reports_missing_acpx_runtime() {
        let empty_path = tempfile::tempdir().expect("empty path dir");
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "opencode".to_string(),
            kind: "acpx".to_string(),
            command: Some("opencode".to_string()),
            env: Some(HashMap::from([(
                "PATH".to_string(),
                empty_path.path().display().to_string(),
            )])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("opencode")
                && finding.message.contains("requires acpx")
        }));
    }

    #[test]
    fn check_agent_wiring_reports_missing_acpx_agent_command() {
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_test_executable(bin_dir.path(), "acpx");
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "opencode".to_string(),
            kind: "acpx".to_string(),
            command: Some("opencode".to_string()),
            env: Some(HashMap::from([(
                "PATH".to_string(),
                bin_dir.path().display().to_string(),
            )])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding.message.contains("opencode")
                && finding.message.contains("acpx runtime found")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("opencode")
                && finding.message.contains("command 'opencode'")
        }));
    }

    #[test]
    fn check_agent_wiring_reports_missing_default_cli_command() {
        let empty_path = tempfile::tempdir().expect("empty path dir");
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            env: Some(HashMap::from([(
                "PATH".to_string(),
                empty_path.path().display().to_string(),
            )])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(check_agent_wiring(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("codex")
                && finding.message.contains("command 'codex'")
        }));
    }

    #[test]
    fn validates_persisted_active_state_against_config() {
        let mut config = base_config();
        config.model_roles.push(ModelRoleConfig {
            role: "balanced".to_string(),
            model: "local-kimi-gpt55".to_string(),
            description: None,
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("active-agents.json"),
            r#"{"brian":"missing-agent"}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("active-models.json"),
            r#"{"brian":"balanced","david":"missing-model"}"#,
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
                    .contains("unknown gateway model selector 'missing-model'")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding
                    .message
                    .contains("active model override for 'brian' points to 'balanced'")
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
    fn openclaw_channel_route_statuses_distinguish_plugin_from_missing_route() {
        assert!(openclaw_channel_route_status_is_present(
            reqwest::StatusCode::METHOD_NOT_ALLOWED
        ));
        assert!(!openclaw_channel_route_status_is_present(
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(!openclaw_channel_route_status_is_present(
            reqwest::StatusCode::FORBIDDEN
        ));
        assert!(openclaw_channel_route_status_is_present(
            reqwest::StatusCode::BAD_REQUEST
        ));
        assert!(!openclaw_channel_route_status_is_present(
            reqwest::StatusCode::NOT_FOUND
        ));
        assert!(!openclaw_channel_route_status_is_present(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
    }

    #[test]
    fn openclaw_channel_route_reports_rejected_configured_token() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _mock = server
                    .mock("GET", "/calciforge/inbound")
                    .with_status(401)
                    .create_async()
                    .await;
                let agent = AgentConfig {
                    id: "custodian".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: server.url(),
                    api_key: Some("wrong-token".to_string()),
                    ..Default::default()
                };
                let mut report = DoctorReport::default();

                check_openclaw_channel_route(&agent, false, &mut report).await;

                assert!(report.findings.iter().any(|finding| {
                    finding.severity == Severity::Error
                        && finding
                            .message
                            .contains("rejected the configured inbound token")
                }));
                assert!(!report.findings.iter().any(|finding| {
                    finding.severity == Severity::Ok
                        && finding.message.contains("exposes openclaw-channel route")
                }));
            });
    }

    #[test]
    fn parses_persisted_install_node_metadata() {
        let nodes = parse_install_nodes_json(
            r#"{
              "nodes": [
                {
                  "name": "gateway",
                  "host": "example.internal",
                  "user": "root",
                  "ssh_key": "/keys/id_ed25519",
                  "os": "linux",
                  "install_dir": "/usr/local/bin",
                  "config_dir": "/etc/calciforge"
                }
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "gateway");
        assert_eq!(nodes[0].host, "example.internal");
        assert_eq!(nodes[0].ssh_key, Some(PathBuf::from("/keys/id_ed25519")));
    }

    #[test]
    fn install_node_permission_command_is_quoted() {
        let node = InstallNodeMetadata {
            name: "quoted".to_string(),
            host: "example.internal".to_string(),
            user: "root".to_string(),
            ssh_key: None,
            os: "linux".to_string(),
            install_dir: "/usr/local/bin; rm -rf /".to_string(),
            config_dir: "/etc/calciforge".to_string(),
        };

        let command = remote_install_node_permission_command(&node).unwrap();
        assert!(command.contains("install_dir='/usr/local/bin; rm -rf /'"));
        assert!(command.contains("test -w \"$dir\""));
    }

    #[test]
    fn install_node_permission_command_rejects_newlines() {
        let node = InstallNodeMetadata {
            name: "bad".to_string(),
            host: "example.internal".to_string(),
            user: "root".to_string(),
            ssh_key: None,
            os: "linux\nuname -a".to_string(),
            install_dir: "/usr/local/bin".to_string(),
            config_dir: "/etc/calciforge".to_string(),
        };

        let err = remote_install_node_permission_command(&node).unwrap_err();
        assert!(err.to_string().contains("control character"));
    }

    #[test]
    fn proxy_environment_accepts_missing_ambient_proxy() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding
                    .message
                    .contains("no ambient HTTP_PROXY/HTTPS_PROXY/ALL_PROXY")
        }));
    }

    #[test]
    fn proxy_environment_skips_no_proxy_check_without_active_proxy() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                http: None,
                https: None,
                no_proxy: None,
                ..Default::default()
            },
            &mut report,
        );

        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.message.contains("NO_PROXY does not include"))
        );
    }

    #[test]
    fn proxy_environment_warns_on_matching_ambient_proxy() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("ambient HTTP(S)_PROXY configured")
        }));
    }

    #[test]
    fn proxy_environment_warns_on_all_proxy_only_and_checks_no_proxy() {
        let mut report = DoctorReport::default();
        check_proxy_environment_in(
            ProxyEnvironment {
                all: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: None,
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn && finding.message.contains("ambient ALL_PROXY set")
        }));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.message.contains("NO_PROXY does not include"))
        );
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
    fn environment_value_extracts_security_proxy_ca_path() {
        let env = "SECURITY_PROXY_PORT=8888 SECURITY_PROXY_CA_CERT=/root/.config/calciforge/secrets/mitm-ca.pem SECURITY_PROXY_CA_KEY=/root/.config/calciforge/secrets/mitm-ca-key.pem";
        assert_eq!(
            environment_value(env, "SECURITY_PROXY_CA_CERT").as_deref(),
            Some("/root/.config/calciforge/secrets/mitm-ca.pem")
        );
    }

    #[test]
    fn environment_value_ignores_empty_values() {
        assert_eq!(
            environment_value("SECURITY_PROXY_CA_CERT=", "SECURITY_PROXY_CA_CERT"),
            None
        );
    }

    #[test]
    fn active_security_proxy_ca_prefers_systemd_unit_over_process_env() {
        let systemd_env =
            "SECURITY_PROXY_PORT=8888 SECURITY_PROXY_CA_CERT=/etc/calciforge/active-ca.pem";

        assert_eq!(
            active_security_proxy_ca_cert_from_values(
                [systemd_env].into_iter(),
                Some("/tmp/stale-shell-ca.pem")
            )
            .as_deref(),
            Some("/etc/calciforge/active-ca.pem")
        );
    }

    #[test]
    fn active_security_proxy_ca_falls_back_to_process_env() {
        assert_eq!(
            active_security_proxy_ca_cert_from_values(
                ["SECURITY_PROXY_PORT=8888"].into_iter(),
                Some("/tmp/shell-ca.pem")
            )
            .as_deref(),
            Some("/tmp/shell-ca.pem")
        );
    }

    #[test]
    fn ca_bundle_verification_rejects_same_subject_stale_ca() {
        if StdCommand::new("openssl").arg("version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().expect("create tempdir");
        let active_cert = temp.path().join("active-ca.pem");
        let active_key = temp.path().join("active-ca-key.pem");
        let stale_cert = temp.path().join("stale-ca.pem");
        let stale_key = temp.path().join("stale-ca-key.pem");
        generate_test_ca(&active_cert, &active_key);
        generate_test_ca(&stale_cert, &stale_key);

        assert!(verify_ca_cert_with_bundle(&active_cert, &active_cert).is_ok());
        assert!(
            verify_ca_cert_with_bundle(&active_cert, &stale_cert).is_err(),
            "a stale same-subject CA must not validate the active MITM CA"
        );
    }

    fn generate_test_ca(cert: &Path, key: &Path) {
        let status = StdCommand::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-days", "1", "-nodes", "-keyout",
            ])
            .arg(key)
            .arg("-out")
            .arg(cert)
            .args([
                "-subj",
                "/CN=Calciforge Local MITM CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run openssl");
        assert!(status.success(), "openssl generated test CA");
    }

    #[test]
    fn subprocess_agent_proxy_coverage_accepts_missing_proxy_env() {
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
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding.message.contains("have no explicit MITM proxy env")
        }));
    }

    #[test]
    fn install_override_requires_agent_egress_proxy() {
        let config = base_config();

        assert!(!security_requires_agent_egress_proxy_with_override(
            &config, None
        ));
        assert!(security_requires_agent_egress_proxy_with_override(
            &config,
            Some("1")
        ));
        assert!(security_requires_agent_egress_proxy_with_override(
            &config,
            Some("yes")
        ));
        assert!(!security_requires_agent_egress_proxy_with_override(
            &config,
            Some("false")
        ));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_warns_on_complete_agent_proxy_env() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "dirac".to_string(),
            kind: "dirac-cli".to_string(),
            env: Some(HashMap::from([
                (
                    "HTTP_PROXY".to_string(),
                    "http://127.0.0.1:8888".to_string(),
                ),
                (
                    "HTTPS_PROXY".to_string(),
                    "http://127.0.0.1:8888".to_string(),
                ),
                ("ALL_PROXY".to_string(), "http://127.0.0.1:8888".to_string()),
                (
                    "NO_PROXY".to_string(),
                    "localhost,127.0.0.1,::1".to_string(),
                ),
                (
                    "NODE_EXTRA_CA_CERTS".to_string(),
                    "/tmp/mitm-ca.pem".to_string(),
                ),
            ])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("define complete MITM proxy env")
        }));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_warns_when_agent_env_is_incomplete() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            env: Some(HashMap::from([(
                "https_proxy".to_string(),
                "http://127.0.0.1:9999".to_string(),
            )])),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("define incomplete MITM proxy env")
        }));
    }

    #[test]
    fn subprocess_agent_proxy_coverage_errors_in_strict_security_without_proxy_env() {
        let mut config = base_config();
        config.security = Some(SecuritySectionConfig {
            profile: "hardened".to_string(),
            scan_outbound: Some(true),
            require_agent_egress_proxy: true,
            scanner_checks: vec![],
        });
        config.agents = vec![AgentConfig {
            id: "codex".to_string(),
            kind: "codex-cli".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.message.contains("have no explicit MITM proxy env")
        }));
    }

    #[test]
    fn scan_outbound_false_relaxes_profile_default_strict_egress() {
        let mut config = base_config();
        config.security = Some(SecuritySectionConfig {
            profile: "hardened".to_string(),
            scan_outbound: Some(false),
            require_agent_egress_proxy: false,
            scanner_checks: vec![],
        });

        assert!(!security_requires_agent_egress_proxy(&config));
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
                http: None,
                https: None,
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("set empty proxy env values")
        }));
    }

    #[test]
    fn external_agent_proxy_coverage_errors_in_strict_security() {
        let mut config = base_config();
        config.security = Some(SecuritySectionConfig {
            profile: "hardened".to_string(),
            scan_outbound: Some(true),
            require_agent_egress_proxy: true,
            scanner_checks: vec![],
        });
        config.agents = vec![AgentConfig {
            id: "openclaw".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://127.0.0.1:18789".to_string(),
            ..Default::default()
        }];
        let mut report = DoctorReport::default();

        check_agent_proxy_coverage(
            &config,
            &ProxyEnvironment {
                http: Some("http://127.0.0.1:8888".to_string()),
                https: Some("http://127.0.0.1:8888".to_string()),
                all: Some("http://127.0.0.1:8888".to_string()),
                no_proxy: Some("localhost,127.0.0.1".to_string()),
                node_extra_ca_certs: Some("/tmp/mitm-ca.pem".to_string()),
                ..Default::default()
            },
            &mut report,
        );

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("doctor cannot verify their process proxy environment")
        }));
    }

    #[test]
    fn external_agent_proxy_coverage_warns_that_daemon_env_is_unverified() {
        let mut config = base_config();
        config.agents = vec![AgentConfig {
            id: "openclaw".to_string(),
            kind: "openclaw-channel".to_string(),
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
                ..Default::default()
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
    fn model_gateway_config_reports_unknown_route_provider() {
        let mut config = base_config();
        let proxy = config.proxy.as_mut().expect("proxy");
        proxy.model_routes = vec![ProxyModelRoute {
            pattern: "opencode-go/kimi-k2.6".to_string(),
            provider: "opencode-go".to_string(),
        }];
        proxy.providers.clear();
        let mut report = DoctorReport::default();

        check_model_gateway_config(&config, &mut report);

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("references unknown provider 'opencode-go'")
        }));
    }

    #[test]
    fn model_gateway_config_loads_configured_provider_routes() {
        let mut config = base_config();
        let proxy = config.proxy.as_mut().expect("proxy");
        proxy.providers = vec![ProxyProviderConfig {
            id: "opencode-go".to_string(),
            backend_type: "http".to_string(),
            url: "https://opencode.example/v1".to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["opencode-go/*".to_string()],
            strip_model_prefix: Some("opencode-go/".to_string()),
            add_model_prefix: None,
            timeout_seconds: Some(60),
            headers: HashMap::new(),
            on_switch: None,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        }];
        proxy.model_routes = vec![ProxyModelRoute {
            pattern: "opencode-go/kimi-k2.6".to_string(),
            provider: "opencode-go".to_string(),
        }];
        let mut report = DoctorReport::default();

        check_model_gateway_config(&config, &mut report);

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding
                    .message
                    .contains("model gateway provider routing loads: 2 route entries")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Warn
                && finding.message.contains("provider 'opencode-go'")
                && finding
                    .message
                    .contains("Calciforge-owned builtin HTTP upstream credentials")
                && finding
                    .message
                    .contains("not handled by an external provider dashboard or registry")
        }));
    }

    #[test]
    fn model_gateway_route_graph_warns_when_selector_falls_through_default_gateway() {
        let mut config = base_config();
        config.dispatchers = vec![crate::config::DispatcherConfig {
            id: "balanced".to_string(),
            name: None,
            models: vec![SyntheticModelConfig {
                model: "qwen3.6:27b".to_string(),
                context_window: 128_000,
            }],
        }];
        let proxy = config.proxy.as_mut().expect("proxy");
        proxy.backend_type = "helicone".to_string();
        proxy.providers = vec![ProxyProviderConfig {
            id: "helicone-ollama".to_string(),
            backend_type: "helicone".to_string(),
            url: "http://127.0.0.1:8787/ai".to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["other-local-model".to_string()],
            strip_model_prefix: None,
            add_model_prefix: Some("ollama/".to_string()),
            timeout_seconds: Some(900),
            headers: HashMap::new(),
            on_switch: Some("/usr/local/bin/calciforge-ollama-switch".to_string()),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        }];
        proxy.model_routes.clear();
        let mut report = DoctorReport::default();

        check_model_gateway_config(&config, &mut report);

        assert!(
            report.findings.iter().any(|finding| {
                finding.severity == Severity::Warn
                    && finding.message.contains("selector 'balanced'")
                    && finding.message.contains("concrete model 'qwen3.6:27b'")
                    && finding.message.contains("default helicone gateway")
            }),
            "doctor should warn when a synthetic selector will bypass explicit provider prefixes/hooks; findings: {:?}",
            report.findings
        );
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok
                && finding.message.contains("1 default gateway fallback route")
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
                ..Default::default()
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
                ..Default::default()
            },
            &mut report,
        );

        assert!(!report.findings.iter().any(|finding| {
            finding
                .message
                .contains("doctor cannot verify their process proxy environment")
        }));
    }

    #[test]
    fn scanner_config_validates_local_policy_rules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let policy = tmp.path().join("scanner.star");
        std::fs::write(&policy, "def scan(input):\n    return \"clean\"\n").unwrap();

        let mut config = base_config();
        config.security = Some(SecuritySectionConfig {
            profile: "hardened".to_string(),
            scan_outbound: Some(true),
            require_agent_egress_proxy: false,
            scanner_checks: vec![ScannerCheckConfig::Starlark {
                path: policy.to_string_lossy().into_owned(),
                fail_closed: true,
                max_callstack: 32,
            }],
        });
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_scanner_config(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Ok && finding.message.contains("starlark policy loads")
        }));
    }

    #[test]
    fn scanner_config_reports_bad_starlark_and_remote_rules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let policy = tmp.path().join("scanner.star");
        std::fs::write(&policy, "BROKEN = True\n").unwrap();

        let mut config = base_config();
        config.security = Some(SecuritySectionConfig {
            profile: "hardened".to_string(),
            scan_outbound: Some(true),
            require_agent_egress_proxy: false,
            scanner_checks: vec![
                ScannerCheckConfig::Starlark {
                    path: policy.to_string_lossy().into_owned(),
                    fail_closed: true,
                    max_callstack: 32,
                },
                ScannerCheckConfig::RemoteHttp {
                    url: "file:///tmp/scanner".to_string(),
                    fail_closed: true,
                },
            ],
        });
        let mut report = DoctorReport::default();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_scanner_config(&config, true, &mut report));

        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("starlark policy failed validation")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding
                    .message
                    .contains("remote_http URL must use http or https")
        }));
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

    fn write_test_executable(dir: &Path, name: &str) -> PathBuf {
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
        path
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

        let env = HashMap::from([("PATH".to_string(), tmp.path().display().to_string())]);
        assert!(find_executable_for_agent("fnox", Some(&env)).is_none());

        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        assert_eq!(find_executable_for_agent("fnox", Some(&env)), Some(path));
    }

    #[test]
    fn fnox_provider_count_ignores_empty_status_lines() {
        assert_eq!(count_fnox_provider_lines("calciforge-local\n"), 1);
        assert_eq!(
            count_fnox_provider_lines("\nProviders:\nkeychain\nage\n"),
            2
        );
        assert_eq!(count_fnox_provider_lines("No providers configured\n"), 0);
        assert_eq!(count_fnox_provider_lines("No provider found\n"), 0);
    }
}
