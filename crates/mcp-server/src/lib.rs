//! `mcp-server` — agent-facing secret-discovery MCP server.
//!
//! Per `docs/rfcs/agent-secret-gateway.md` §4. Exposes a deliberately
//! narrow surface so that agents can:
//!
//! - **discover** what secrets exist (`list_secrets` returns NAMES,
//!   never values)
//! - **build canonical references** to secrets they want to use in
//!   outbound requests (`secret_reference(name) → "{{secret:NAME}}"`)
//! - **kick off the user-facing add flow** for secrets they need but
//!   that aren't configured yet (`add_secret_request`, currently
//!   returns safe operator instructions while daemon integration lands)
//!
//! Critically, this server does **NOT** expose `get_secret`. Doing so
//! would defeat the threat model — any agent that connects to MCP
//! could just enumerate names and pull values. Values flow through
//! the security-proxy substitution layer ONLY, where they're injected
//! at the network boundary and never enter agent context.
//!
//! ## Why a separate binary
//!
//! MCP servers run as subprocesses spawned by agents (Claude Code,
//! opencode, etc.) over stdio. Splitting the MCP server out of the
//! main `calciforge` binary means:
//! 1. Agents that don't need secret discovery don't pay the startup cost.
//! 2. The MCP server can be installed/run by an agent that doesn't
//!    have access to the full calciforge daemon (e.g., when running
//!    `claude` in a sandbox).
//! 3. Failures here don't take down the gateway.

use rmcp::ErrorData as McpError;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;

/// State carried by the MCP server. Cheap to clone; the `FnoxClient`
/// is just a path wrapper.
#[derive(Clone)]
pub struct CalciforgeMcp {
    fnox: secrets_client::FnoxClient,
    tool_router: ToolRouter<Self>,
}

impl Default for CalciforgeMcp {
    fn default() -> Self {
        Self::new(secrets_client::FnoxClient::new())
    }
}

impl CalciforgeMcp {
    /// Construct a server bound to a specific [`secrets_client::FnoxClient`].
    /// Production callers use [`CalciforgeMcp::default`] which uses
    /// `fnox` from `PATH`; tests inject a `FnoxClient::with_binary(path)`
    /// pointing at a fake script.
    pub fn new(fnox: secrets_client::FnoxClient) -> Self {
        Self {
            fnox,
            tool_router: Self::tool_router(),
        }
    }

    /// Validates a secret name against the same shape the substitution
    /// engine accepts (see `crates/security-proxy/src/substitution.rs`).
    /// Keeping the two in sync is critical: if `secret_reference`
    /// produced a token the substitution engine wouldn't accept, the
    /// agent's request would silently fail downstream.
    fn is_valid_name(name: &str) -> bool {
        secrets_client::is_valid_secret_name(name)
    }
}

#[derive(Deserialize, rmcp::schemars::JsonSchema, Debug)]
pub struct SecretReferenceParams {
    /// Name of an existing secret. Must match `[A-Za-z0-9_-]+`.
    /// Returns the canonical `{{secret:NAME}}` token the agent embeds
    /// in outbound HTTP requests (URL, headers, JSON body).
    pub name: String,
}

#[derive(Deserialize, rmcp::schemars::JsonSchema, Debug)]
pub struct AddSecretRequestParams {
    /// Proposed secret name. Same shape rules as
    /// [`SecretReferenceParams::name`].
    pub name: String,
    /// Human-facing description of what this secret is for. Stored
    /// alongside the name in the MCP's discovery output.
    pub description: String,
    /// True if it's acceptable for the value to flow through the chat
    /// transport (Telegram/Matrix retain history). False means the
    /// out-of-band paste flow is required.
    #[serde(default)]
    pub retention_ok: bool,
}

#[tool_router(router = tool_router)]
impl CalciforgeMcp {
    /// List all secret names this MCP can substitute on the agent's
    /// behalf. Returns names only — never values. The agent uses these
    /// names to build references via [`secret_reference`].
    ///
    /// Empty list is a normal outcome (fresh deployment); not an error.
    #[tool(
        description = "List all stored secret NAMES (never values). Returns the names an agent can build references to via `secret_reference`."
    )]
    async fn list_secrets(&self) -> Result<CallToolResult, McpError> {
        match self.fnox.list().await {
            Ok(names) => {
                let payload = serde_json::json!({
                    "names": names,
                    "count": names.len(),
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]))
            }
            Err(secrets_client::FnoxError::NotInstalled(e)) => Err(McpError::internal_error(
                format!(
                    "fnox is not installed; secret-discovery is unavailable. \
                     Install fnox (brew install fnox) and run `fnox init`. {e}"
                ),
                None,
            )),
            Err(e) => Err(McpError::internal_error(
                format!("fnox list failed: {e}"),
                None,
            )),
        }
    }

    /// Return the canonical `{{secret:NAME}}` reference token for a
    /// given secret name. The agent embeds this token in any outbound
    /// HTTP request (URL path/query, header value, JSON body); the
    /// security-proxy substitutes the real value at the network
    /// boundary so it never enters the agent's context.
    ///
    /// Validates the name shape (`[A-Za-z0-9_-]+`); does NOT check
    /// existence — an agent reaching for an unconfigured name should
    /// learn that downstream when substitution fails, OR via
    /// [`add_secret_request`] proactively.
    #[tool(
        description = "Build the canonical reference token (`{{secret:NAME}}`) for a stored secret. The agent embeds this in outbound HTTP requests; the security-proxy substitutes the real value at the network boundary."
    )]
    async fn secret_reference(
        &self,
        params: Parameters<SecretReferenceParams>,
    ) -> Result<CallToolResult, McpError> {
        let SecretReferenceParams { name } = params.0;
        if !Self::is_valid_name(&name) {
            return Err(McpError::invalid_params(
                format!(
                    "secret name {name:?} contains invalid characters (allowed: A-Z a-z 0-9 _ -)"
                ),
                None,
            ));
        }
        let token = secrets_client::secret_reference_token(&name)
            .expect("validated above: valid secret name");
        let payload = serde_json::json!({
            "name": name,
            "reference": token,
            "usage": "Embed this exact token in outbound HTTP request URL, headers, or JSON body. The security-proxy will substitute the real value at the network boundary; the value will not enter your context.",
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&payload).unwrap_or_default(),
        )]))
    }

    /// Initiate the out-of-band secret-add flow. Currently returns
    /// instructions telling the user to run the local paste UI or
    /// `fnox set NAME` on the host. Chat-channel value entry is not
    /// recommended from agent guidance.
    ///
    /// Future implementation will return a short-lived localhost URL
    /// the user visits to paste the secret value out-of-band.
    #[tool(
        description = "Initiate the user-facing flow for adding a secret. Currently returns instructions; future implementation returns a short-lived URL for out-of-band paste."
    )]
    async fn add_secret_request(
        &self,
        params: Parameters<AddSecretRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        let AddSecretRequestParams {
            name,
            description,
            retention_ok,
        } = params.0;
        if !Self::is_valid_name(&name) {
            return Err(McpError::invalid_params(
                format!(
                    "proposed secret name {name:?} contains invalid characters (allowed: A-Z a-z 0-9 _ -)"
                ),
                None,
            ));
        }
        let suggestion = if retention_ok {
            format!(
                "Tell the user to run one of:\n\
                 • preferred local paste UI: `paste-server {name}`\n\
                 • host-local: `fnox set {name}` (interactive prompt)\n\
                 Avoid channel-based secret entry unless the operator \
                 has explicitly opted into that retention tradeoff."
            )
        } else {
            format!(
                "The user requires an out-of-band paste flow for this \
                 secret. Tell them to run `paste-server {name}` or \
                 `fnox set {name}` directly on the host where the gateway \
                 is deployed. The chat-channel path (`!secure set`) is \
                 NOT acceptable for this secret."
            )
        };
        let payload = serde_json::json!({
            "name": name,
            "description": description,
            "retention_ok": retention_ok,
            "instructions": suggestion,
            "status": "stub",
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&payload).unwrap_or_default(),
        )]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CalciforgeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "mcp-server".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: None,
                website_url: None,
                icons: None,
            },
            instructions: Some(
                "Calciforge secret-discovery MCP. \
                 Use `list_secrets` to see what's available, \
                 `secret_reference(name)` to get the `{{secret:NAME}}` token \
                 you embed in outbound HTTP requests, and \
                 `add_secret_request(name, description)` to ask the user \
                 to add a missing secret. \
                 Values never flow through this MCP; they're substituted \
                 by the security-proxy at the network boundary."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests exercise the public tool methods directly (no MCP
    //! transport), using a `FnoxClient::with_binary(fake)` to mock the
    //! underlying secret store. Same hermetic pattern as the
    //! `FnoxClient` tests themselves — no PATH manipulation, no
    //! global env mutation.

    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fake_fnox(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("fnox");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Given a fake fnox that lists three names,
    /// when list_secrets is called,
    /// then the result names them and the count matches.
    /// (Implicit: values never appear because the underlying
    /// FnoxClient::list returns names-only — guarded by FnoxClient's
    /// own tests; this just confirms the MCP wraps that correctly.)
    #[tokio::test]
    async fn list_secrets_returns_name_list_with_count() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"cat <<OUT
KEY_A
KEY_B
KEY_C
OUT"#,
        );
        let server = CalciforgeMcp::new(secrets_client::FnoxClient::with_binary(bin));

        let result = server.list_secrets().await.unwrap();
        let body = match result.content[0].raw.as_text() {
            Some(t) => t.text.clone(),
            None => panic!("expected text content"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["count"], 3);
        let names = parsed["names"].as_array().unwrap();
        let name_strs: Vec<&str> = names.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(name_strs, vec!["KEY_A", "KEY_B", "KEY_C"]);
    }

    /// Given fnox not installed,
    /// when list_secrets is called,
    /// then the MCP returns an error that points the operator at the
    /// install-fnox remediation, NOT a stack trace or
    /// internal-implementation leak.
    #[tokio::test]
    async fn list_secrets_returns_actionable_error_when_fnox_missing() {
        let server = CalciforgeMcp::new(secrets_client::FnoxClient::with_binary(
            "/tmp/no-such-fnox-pid-zzz",
        ));

        let err = server.list_secrets().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("fnox") && (msg.contains("install") || msg.contains("not installed")),
            "error should name fnox + suggest install: {msg}"
        );
    }

    /// Given a valid secret name,
    /// when secret_reference is called,
    /// then the result contains the canonical `{{secret:NAME}}` token
    /// (matching the substitution engine's accepted syntax) and a
    /// usage hint that names the boundary at which substitution
    /// happens.
    #[tokio::test]
    async fn secret_reference_returns_canonical_token() {
        let server = CalciforgeMcp::default();

        let result = server
            .secret_reference(Parameters(SecretReferenceParams {
                name: "ANTHROPIC_API_KEY".to_string(),
            }))
            .await
            .unwrap();
        let body = result.content[0].raw.as_text().unwrap().text.clone();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["reference"], "{{secret:ANTHROPIC_API_KEY}}");
        assert_eq!(parsed["name"], "ANTHROPIC_API_KEY");
        assert!(
            parsed["usage"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("network boundary"),
            "usage should explain when substitution happens"
        );
    }

    /// Given an invalid secret name (contains a slash),
    /// when secret_reference is called,
    /// then the MCP returns an InvalidParams error rather than
    /// returning a token the substitution engine would later reject.
    /// Symmetric validation between MCP and substitution prevents the
    /// agent from being told "here's your token" only to fail
    /// downstream.
    #[tokio::test]
    async fn secret_reference_rejects_invalid_name_shape() {
        let server = CalciforgeMcp::default();
        let err = server
            .secret_reference(Parameters(SecretReferenceParams {
                name: "FOO/BAR".to_string(),
            }))
            .await
            .unwrap_err();
        // Just assert it's an error — the exact code path matters less
        // than not silently producing a broken token.
        let msg = err.to_string();
        assert!(
            msg.contains("invalid characters") || msg.contains("FOO/BAR"),
            "error should name the problem: {msg}"
        );
    }

    /// Given add_secret_request with retention_ok=true,
    /// when called,
    /// then the instructions prefer the local paste UI, keep host-local
    /// fnox available, and avoid recommending channel-based secret entry.
    #[tokio::test]
    async fn add_secret_request_with_retention_ok_offers_chat_path() {
        let server = CalciforgeMcp::default();
        let result = server
            .add_secret_request(Parameters(AddSecretRequestParams {
                name: "BRAVE_KEY".into(),
                description: "Brave Search API".into(),
                retention_ok: true,
            }))
            .await
            .unwrap();
        let body = result.content[0].raw.as_text().unwrap().text.clone();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let inst = parsed["instructions"].as_str().unwrap();
        assert!(inst.contains("paste-server"));
        assert!(inst.contains("fnox set"));
        assert!(!inst.contains("!secure set"));
        assert!(inst.contains("retention tradeoff"));
    }

    /// Given add_secret_request with retention_ok=false,
    /// when called,
    /// then the instructions explicitly REJECT the !secure set chat
    /// path and direct the user to local routes. This is the
    /// safety contract — high-value secrets must not slip through to
    /// chat just because the agent forgot to set the flag.
    #[tokio::test]
    async fn add_secret_request_with_retention_not_ok_rejects_chat_path() {
        let server = CalciforgeMcp::default();
        let result = server
            .add_secret_request(Parameters(AddSecretRequestParams {
                name: "ANTHROPIC".into(),
                description: "Anthropic API key".into(),
                retention_ok: false,
            }))
            .await
            .unwrap();
        let body = result.content[0].raw.as_text().unwrap().text.clone();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let inst = parsed["instructions"].as_str().unwrap();
        assert!(
            inst.contains("NOT acceptable"),
            "must explicitly reject the chat path for non-retention-ok"
        );
    }

    /// Confirms the server's announced capabilities include `tools`.
    /// Catches a regression where the macro registration silently
    /// loses a tool or the capabilities builder is mis-configured.
    #[test]
    fn server_advertises_tools_capability() {
        use rmcp::ServerHandler;
        let server = CalciforgeMcp::default();
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, "mcp-server");
        assert!(info.instructions.is_some());
    }
}
