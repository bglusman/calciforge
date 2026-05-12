use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::DoctorReport;

#[derive(Deserialize)]
struct SecurityProxyHealth {
    service: Option<String>,
}

pub(super) async fn check(report: &mut DoctorReport) {
    let Some(url) = expected_health_url() else {
        report.ok("no local managed security-proxy runtime metadata found; listener check skipped");
        return;
    };

    check_health_url(&url, report).await;
}

async fn check_health_url(url: &str, report: &mut DoctorReport) {
    let parsed = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(err) => {
            report.error(format!("security-proxy health URL is invalid: {err}"));
            return;
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            report.error(format!(
                "security-proxy health check could not build HTTP client: {err}"
            ));
            return;
        }
    };

    match client.get(parsed.clone()).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<SecurityProxyHealth>().await {
                Ok(health) if health.service.as_deref() == Some("security-gateway") => {
                    report.ok(format!("security-proxy listener is healthy at {parsed}"));
                }
                Ok(_) => {
                    report.error(format!(
                    "listener at {parsed} returned a health response, but it did not identify Calciforge security-proxy"
                ));
                }
                Err(err) => {
                    report.error(format!(
                    "listener at {parsed} returned success, but its health response was not recognized: {err}"
                ));
                }
            }
        }
        Ok(response) => {
            report.error(format!(
                "security-proxy listener at {parsed} returned {}",
                response.status()
            ));
        }
        Err(err) => {
            report.error(format!(
                "security-proxy listener is not reachable at {parsed}: {err}"
            ));
        }
    }
}

fn expected_health_url() -> Option<String> {
    let service_env = local_runtime_env();
    if let Some(url) =
        env_security_proxy_url().or_else(|| service_env_value(&service_env, "SECURITY_PROXY_URL"))
    {
        return Some(format!("{}/health", url.trim_end_matches('/')));
    }

    if service_env.is_empty() && !local_runtime_expected() {
        return None;
    }

    let bind = std::env::var("SECURITY_PROXY_BIND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| service_env_value(&service_env, "SECURITY_PROXY_BIND"))
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = std::env::var("SECURITY_PROXY_PORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| service_env_value(&service_env, "SECURITY_PROXY_PORT"))
        .unwrap_or_else(|| "8888".to_string());
    let host = match bind.as_str() {
        "0.0.0.0" | "::" => "127.0.0.1",
        other => other.trim_matches(['[', ']']),
    };

    Some(format!("http://{host}:{port}/health"))
}

fn env_security_proxy_url() -> Option<String> {
    std::env::var("SECURITY_PROXY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn local_runtime_expected() -> bool {
    local_launch_agent_exists() || local_systemd_unit_exists()
}

fn local_runtime_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for path in local_launch_agent_paths() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            env.extend(parse_launch_agent_env(&contents));
        }
    }
    for path in local_systemd_unit_paths() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            env.extend(parse_systemd_env(&contents));
        }
    }
    env
}

fn service_env_value(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter()
        .rev()
        .find_map(|(env_key, value)| (env_key == key).then(|| value.clone()))
}

fn local_launch_agent_paths() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| vec![home.join("Library/LaunchAgents/com.calciforge.security-proxy.plist")])
        .unwrap_or_default()
}

fn local_launch_agent_exists() -> bool {
    local_launch_agent_paths().iter().any(|path| path.exists())
}

fn local_systemd_unit_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".config/systemd/user/calciforge-security-proxy.service"));
    }
    paths.push(PathBuf::from(
        "/etc/systemd/system/calciforge-security-proxy.service",
    ));
    paths
}

fn local_systemd_unit_exists() -> bool {
    local_systemd_unit_paths().iter().any(|path| path.exists())
}

fn parse_launch_agent_env(contents: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut pending_key = None;
    for line in contents.lines() {
        if let Some(key) =
            plist_tag_value(line, "key").filter(|key| key.starts_with("SECURITY_PROXY_"))
        {
            pending_key = Some(key);
        }
        if let (Some(key), Some(value)) = (pending_key.take(), plist_tag_value(line, "string")) {
            env.push((key, value));
        }
    }
    env
}

fn plist_tag_value(line: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = line.find(&open)? + open.len();
    let end = line[start..].find(&close)? + start;
    Some(line[start..end].to_string())
}

fn parse_systemd_env(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Environment="))
        .flat_map(|value| value.split_whitespace())
        .filter_map(|part| part.trim_matches('"').split_once('='))
        .filter(|(key, value)| key.starts_with("SECURITY_PROXY_") && !value.is_empty())
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Severity;

    #[test]
    fn health_reports_unreachable_listener() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut report = DoctorReport::default();

                check_health_url("http://127.0.0.1:9/health", &mut report).await;

                assert!(report.findings.iter().any(|finding| {
                    finding.severity == Severity::Error
                        && finding
                            .message
                            .contains("security-proxy listener is not reachable")
                }));
            });
    }

    #[test]
    fn health_accepts_successful_listener() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _mock = server
                    .mock("GET", "/health")
                    .with_status(200)
                    .with_body(r#"{"status":"ok","service":"security-gateway"}"#)
                    .create_async()
                    .await;
                let mut report = DoctorReport::default();

                check_health_url(&format!("{}/health", server.url()), &mut report).await;

                assert!(report.findings.iter().any(|finding| {
                    finding.severity == Severity::Ok
                        && finding
                            .message
                            .contains("security-proxy listener is healthy")
                }));
            });
    }

    #[test]
    fn health_rejects_unrecognized_successful_listener() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _mock = server
                    .mock("GET", "/health")
                    .with_status(200)
                    .with_body(r#"{"status":"ok","service":"something-else"}"#)
                    .create_async()
                    .await;
                let mut report = DoctorReport::default();

                check_health_url(&format!("{}/health", server.url()), &mut report).await;

                assert!(report.findings.iter().any(|finding| {
                    finding.severity == Severity::Error
                        && finding
                            .message
                            .contains("did not identify Calciforge security-proxy")
                }));
            });
    }

    #[test]
    fn parses_launch_agent_security_proxy_env() {
        let env = parse_launch_agent_env(
            r#"
            <key>SECURITY_PROXY_PORT</key><string>18888</string>
            <key>SECURITY_PROXY_BIND</key><string>0.0.0.0</string>
            <key>PATH</key><string>/usr/bin</string>
            "#,
        );

        assert_eq!(
            service_env_value(&env, "SECURITY_PROXY_PORT").as_deref(),
            Some("18888")
        );
        assert_eq!(
            service_env_value(&env, "SECURITY_PROXY_BIND").as_deref(),
            Some("0.0.0.0")
        );
        assert_eq!(service_env_value(&env, "PATH"), None);
    }

    #[test]
    fn parses_systemd_security_proxy_env() {
        let env = parse_systemd_env(
            r#"
            [Service]
            Environment="SECURITY_PROXY_PORT=18888" "SECURITY_PROXY_BIND=127.0.0.2" "PATH=/usr/bin"
            "#,
        );

        assert_eq!(
            service_env_value(&env, "SECURITY_PROXY_PORT").as_deref(),
            Some("18888")
        );
        assert_eq!(
            service_env_value(&env, "SECURITY_PROXY_BIND").as_deref(),
            Some("127.0.0.2")
        );
        assert_eq!(service_env_value(&env, "PATH"), None);
    }
}
