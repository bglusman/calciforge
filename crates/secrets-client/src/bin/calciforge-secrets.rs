//! `calciforge-secrets` — non-MCP secret discovery CLI.
//!
//! This exposes the same safe surface as the MCP server: list names and build
//! placeholder references. It never resolves or prints secret values.

use secrets_client::{
    FnoxClient, SecretAccessIdentity, SecretAccessPolicy, secret_reference_token,
};
use serde::Deserialize;
use serde_json::json;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => {
            let json = args.any(|arg| arg == "--json");
            list(json).await
        }
        Some("set") => {
            let name = args
                .next()
                .ok_or_else(|| "usage: calciforge-secrets set NAME --stdin".to_string())?;
            let stdin_flag = args
                .next()
                .ok_or_else(|| "usage: calciforge-secrets set NAME --stdin".to_string())?;
            if stdin_flag != "--stdin" {
                return Err("usage: calciforge-secrets set NAME --stdin".to_string());
            }
            if args.next().is_some() {
                return Err("usage: calciforge-secrets set NAME --stdin".to_string());
            }
            set(&name).await
        }
        Some("ref") | Some("reference") => {
            let name = args
                .next()
                .ok_or_else(|| "usage: calciforge-secrets ref NAME".to_string())?;
            reference(&name).await
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown command {other:?}\n\nRun `calciforge-secrets help`."
        )),
    }
}

async fn list(json_output: bool) -> Result<(), String> {
    if let Some(remote) = RemoteSecretsApi::from_env() {
        let response = remote.list().await?;
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&response)
                    .map_err(|e| format!("failed to render secret list as JSON: {e}"))?
            );
        } else {
            for secret in response.metadata {
                if secret.allowed_destinations.is_empty() {
                    println!("{}", secret.name);
                } else {
                    println!(
                        "{}\tallowed_destinations={}",
                        secret.name,
                        secret.allowed_destinations.join(",")
                    );
                }
            }
        }
        return Ok(());
    }

    let (policy, identity) = access_context()?;
    let names = FnoxClient::new()
        .list()
        .await
        .map_err(|e| format!("fnox list failed: {e}"))?;
    let names = policy.filter_names(&identity, names);
    let metadata = secrets_client::metadata::metadata_for_names(&names)
        .map_err(|e| format!("secret metadata unavailable: {e}"))?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "secrets": names, "metadata": metadata }))
                .map_err(|e| format!("failed to render secret list as JSON: {e}"))?
        );
    } else {
        for secret in metadata {
            if secret.allowed_destinations.is_empty() {
                println!("{}", secret.name);
            } else {
                println!(
                    "{}\tallowed_destinations={}",
                    secret.name,
                    secret.allowed_destinations.join(",")
                );
            }
        }
    }
    Ok(())
}

async fn set(name: &str) -> Result<(), String> {
    use tokio::io::AsyncReadExt;

    let mut stdin = tokio::io::stdin();
    let mut value = String::new();
    stdin
        .read_to_string(&mut value)
        .await
        .map_err(|e| format!("failed to read secret value from stdin: {e}"))?;
    trim_default_stdin_newline(&mut value);
    if value.is_empty() {
        return Err("refusing to store an empty secret value".into());
    }
    if RemoteSecretsApi::from_env().is_some() {
        return Err("remote calciforge-secrets is read-only; use the operator secret-input UI or a local fnox-backed helper to set secrets".into());
    }
    FnoxClient::new()
        .set(name, &value)
        .await
        .map_err(|e| format!("fnox set failed: {e}"))?;
    eprintln!("stored secret {name}");
    Ok(())
}

fn trim_default_stdin_newline(value: &mut String) {
    if value.ends_with("\r\n") {
        value.truncate(value.len() - 2);
    } else if value.ends_with('\n') {
        value.pop();
    }
}

fn local_reference(name: &str) -> Result<String, String> {
    let token = secret_reference_token(name).ok_or_else(|| {
        format!("invalid secret name {name:?}; allowed characters: A-Z a-z 0-9 _ -")
    })?;
    Ok(token)
}

async fn reference(name: &str) -> Result<(), String> {
    let local_token = local_reference(name)?;
    if let Some(remote) = RemoteSecretsApi::from_env() {
        println!("{}", remote.reference(name).await?);
        return Ok(());
    }

    let (policy, identity) = access_context()?;
    ensure_policy_allows_reference(&policy, &identity, name)?;
    println!("{local_token}");
    Ok(())
}

fn ensure_policy_allows_reference(
    policy: &SecretAccessPolicy,
    identity: &SecretAccessIdentity,
    name: &str,
) -> Result<(), String> {
    if policy.allows(identity, name) {
        return Ok(());
    }
    Err(format!(
        "secret {name:?} is not allowed for the current Calciforge identity"
    ))
}

fn access_context() -> Result<(SecretAccessPolicy, SecretAccessIdentity), String> {
    let policy = secrets_client::load_default_access_policy()
        .map_err(|e| format!("secret access policy unavailable: {e}"))?;
    let identity = SecretAccessIdentity::from_env();
    Ok((policy, identity))
}

#[derive(Clone)]
struct RemoteSecretsApi {
    base_url: String,
    token: Option<String>,
    identity: SecretAccessIdentity,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct ListResponse {
    secrets: Vec<String>,
    #[serde(default)]
    metadata: Vec<secrets_client::SecretMetadata>,
}

#[derive(Debug, Deserialize)]
struct ReferenceResponse {
    reference: String,
}

impl RemoteSecretsApi {
    fn from_env() -> Option<Self> {
        let base_url = std::env::var("CALCIFORGE_SECRETS_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        let token = std::env::var("CALCIFORGE_SECRETS_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Some(Self {
            base_url,
            token,
            identity: SecretAccessIdentity::from_env(),
            client: reqwest::Client::new(),
        })
    }

    async fn list(&self) -> Result<ListResponse, String> {
        let mut response = self
            .send(
                self.client
                    .get(format!("{}/control/secrets/list", self.base_url)),
            )
            .await?
            .json::<ListResponse>()
            .await
            .map_err(|e| format!("invalid Calciforge secret-list response: {e}"))?;
        if response.metadata.is_empty() {
            response.metadata = response
                .secrets
                .iter()
                .map(|name| secrets_client::SecretMetadata {
                    name: name.clone(),
                    allowed_destinations: Vec::new(),
                })
                .collect();
        }
        Ok(response)
    }

    async fn reference(&self, name: &str) -> Result<String, String> {
        let response = self
            .send(
                self.client
                    .get(format!("{}/control/secrets/ref/{name}", self.base_url)),
            )
            .await?
            .json::<ReferenceResponse>()
            .await
            .map_err(|e| format!("invalid Calciforge secret-reference response: {e}"))?;
        Ok(response.reference)
    }

    fn validate_base_url(&self) -> Result<(), String> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|e| format!("invalid Calciforge secret API base URL: {e}"))?;
        match url.scheme() {
            "https" => Ok(()),
            "http" if is_loopback_host(url.host_str()) => Ok(()),
            "http" => Err("refusing to send Calciforge secret API bearer token over plain HTTP except to loopback".into()),
            scheme => Err(format!(
                "unsupported Calciforge secret API URL scheme {scheme:?}; use HTTPS or loopback HTTP"
            )),
        }
    }

    async fn send(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, String> {
        self.validate_base_url()?;
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(agent_id) = &self.identity.agent_id {
            request = request.header("x-calciforge-agent-id", agent_id);
        }
        if let Some(user_id) = &self.identity.user_id {
            request = request.header("x-calciforge-user-id", user_id);
        }
        if let Some(channel_id) = &self.identity.channel {
            request = request.header("x-calciforge-channel-id", channel_id);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Calciforge secret API request failed: {e}"))?;
        if response.status().is_success() {
            Ok(response)
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(format!(
                "Calciforge secret API returned HTTP {status}: {body}"
            ))
        }
    }
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "::1"))
}

fn print_help() {
    println!(
        "calciforge-secrets\n\
         \n\
         Safe secret discovery without MCP. Never prints values.\n\
         \n\
         Commands:\n\
           list [--json]\n\
                      List stored fnox secret names and destination policy\n\
           ref NAME   Print the canonical {{secret:NAME}} placeholder\n\
           set NAME --stdin\n\
                      Store a secret value read from stdin\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    async fn one_shot_http(
        response_body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn remote_list_uses_central_api_with_bearer_token() {
        let (base_url, request) = one_shot_http(r#"{"secrets":["API_KEY","DB_PASS"]}"#).await;
        let api = RemoteSecretsApi {
            base_url,
            token: Some("test-token".into()),
            identity: SecretAccessIdentity {
                agent_id: Some("research-agent".into()),
                user_id: Some("brian".into()),
                channel: Some("signal".into()),
            },
            client: test_client(),
        };

        let response = api.list().await.unwrap();

        assert_eq!(response.secrets, vec!["API_KEY", "DB_PASS"]);
        assert_eq!(
            response.metadata,
            vec![
                secrets_client::SecretMetadata {
                    name: "API_KEY".into(),
                    allowed_destinations: Vec::new()
                },
                secrets_client::SecretMetadata {
                    name: "DB_PASS".into(),
                    allowed_destinations: Vec::new()
                }
            ]
        );
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /control/secrets/list "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token"),
            "request should authenticate to central Calciforge API: {request}"
        );
        assert!(
            request.contains("x-calciforge-agent-id: research-agent"),
            "request should forward Calciforge agent identity: {request}"
        );
        assert!(
            request.contains("x-calciforge-user-id: brian"),
            "request should forward Calciforge user identity: {request}"
        );
        assert!(
            request.contains("x-calciforge-channel-id: signal"),
            "request should forward Calciforge channel identity: {request}"
        );
    }

    #[tokio::test]
    async fn remote_secret_api_rejects_cleartext_non_loopback_base_url() {
        let api = RemoteSecretsApi {
            base_url: "http://calciforge.example".into(),
            token: Some("test-token".into()),
            identity: SecretAccessIdentity::default(),
            client: test_client(),
        };

        let err = api.list().await.unwrap_err();
        assert!(
            err.contains("plain HTTP"),
            "remote helper must fail closed before sending bearer token: {err}"
        );
    }

    #[tokio::test]
    async fn remote_reference_uses_central_api_for_policy_decision() {
        let (base_url, request) =
            one_shot_http(r#"{"reference":"{{secret:BRAVE_API_KEY}}"}"#).await;
        let api = RemoteSecretsApi {
            base_url,
            token: Some("test-token".into()),
            identity: SecretAccessIdentity {
                agent_id: Some("research-agent".into()),
                ..Default::default()
            },
            client: test_client(),
        };

        let reference = api.reference("BRAVE_API_KEY").await.unwrap();

        assert_eq!(reference, "{{secret:BRAVE_API_KEY}}");
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /control/secrets/ref/BRAVE_API_KEY "));
        assert!(
            request.contains("x-calciforge-agent-id: research-agent"),
            "reference request should forward Calciforge identity: {request}"
        );
    }

    #[test]
    fn local_reference_rejects_invalid_secret_names() {
        assert_eq!(local_reference("API_KEY").unwrap(), "{{secret:API_KEY}}");
        assert!(local_reference("../API_KEY").is_err());
    }

    #[test]
    fn trim_default_stdin_newline_removes_one_shell_newline() {
        let mut value = "token\n".to_string();
        trim_default_stdin_newline(&mut value);
        assert_eq!(value, "token");

        let mut value = "token\r\n".to_string();
        trim_default_stdin_newline(&mut value);
        assert_eq!(value, "token");

        let mut value = "token\n\n".to_string();
        trim_default_stdin_newline(&mut value);
        assert_eq!(value, "token\n");
    }
}
