//! ACPX adapter — uses acpx CLI for Agent Communication Protocol
//!
//! Unlike the sacp-based ACP adapter, this uses the acpx binary which handles
//! protocol version translation and session management.
//!
//! Supports both one-shot (exec) and persistent session modes.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info};

use crate::adapters::{AdapterError, AgentAdapter, DispatchContext};

/// ACPX adapter — wraps acpx CLI for ACP agent communication
pub struct AcpxAdapter {
    agent_name: String,
    _args: Vec<String>,
    env: HashMap<String, String>,
    timeout_ms: u64,
    session_dir: PathBuf,
}

impl AcpxAdapter {
    /// Create a new ACPX adapter
    pub fn new(
        agent_name: String,
        args: Option<Vec<String>>,
        env: Option<HashMap<String, String>>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            agent_name,
            _args: args.unwrap_or_default(),
            env: env.unwrap_or_default(),
            timeout_ms: timeout_ms.unwrap_or(300_000),
            session_dir: PathBuf::from("/tmp/acpx-sessions"),
        }
    }

    /// Ensure session directory exists
    async fn ensure_session_dir(&self) -> Result<(), AdapterError> {
        tokio::fs::create_dir_all(&self.session_dir)
            .await
            .map_err(|e| AdapterError::Unavailable(format!("Failed to create session dir: {}", e)))
    }

    /// List existing sessions for this agent
    async fn list_sessions(&self) -> Result<Vec<String>, AdapterError> {
        self.ensure_session_dir().await?;

        let output = Command::new("acpx")
            .arg(&self.agent_name)
            .arg("sessions")
            .arg("list")
            .current_dir(&self.session_dir)
            .envs(&self.env)
            .output()
            .await
            .map_err(|e| AdapterError::Unavailable(format!("Failed to list sessions: {}", e)))?;

        if !output.status.success() {
            // No sessions or error — return empty
            return Ok(vec![]);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let sessions: Vec<String> = stdout
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("No sessions"))
            .map(|s| s.to_string())
            .collect();

        Ok(sessions)
    }

    /// Create or ensure session exists
    async fn ensure_session(&self, session_name: &str) -> Result<(), AdapterError> {
        self.ensure_session_dir().await?;

        // Check if session exists
        let sessions = self.list_sessions().await?;
        if sessions.iter().any(|s| s.contains(session_name)) {
            debug!(session = %session_name, "Session already exists");
            return Ok(());
        }

        // Create new session
        info!(session = %session_name, "Creating new acpx session");
        let output = Command::new("acpx")
            .arg(&self.agent_name)
            .arg("sessions")
            .arg("new")
            .current_dir(&self.session_dir)
            .envs(&self.env)
            .output()
            .await
            .map_err(|e| AdapterError::Unavailable(format!("Failed to create session: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AdapterError::Protocol(format!(
                "Failed to create session: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Strip acpx protocol noise — keep only clean assistant text lines
    fn strip_acpx_noise(raw: &str) -> String {
        raw.lines()
            .filter(|line| {
                let t = line.trim();
                if t.is_empty() {
                    return false;
                }
                // Drop protocol scaffolding lines
                if t.starts_with("[client]") {
                    return false;
                }
                if t.starts_with("[tool]") {
                    return false;
                }
                if t.starts_with("[thinking]") {
                    return false;
                }
                if t.starts_with("[done]") {
                    return false;
                }
                if t.starts_with("[error]") {
                    return false;
                }
                true
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    /// Execute one-shot prompt (no session persistence)
    async fn exec_prompt(&self, message: &str) -> Result<String, AdapterError> {
        self.ensure_session_dir().await?;

        info!(agent = %self.agent_name, "Running acpx exec");

        let mut cmd = Command::new("acpx");
        cmd.arg("--format")
            .arg("text")
            .arg(&self.agent_name)
            .arg("exec")
            .arg(message)
            .current_dir(&self.session_dir)
            .envs(&self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add timeout
        let timeout = std::time::Duration::from_millis(self.timeout_ms);

        let child = cmd
            .spawn()
            .map_err(|e| AdapterError::Unavailable(format!("Failed to spawn acpx: {}", e)))?;

        // Wait with timeout
        let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(AdapterError::Protocol(format!(
                        "acpx exec failed: {}",
                        stderr
                    )));
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(Self::strip_acpx_noise(&stdout))
            }
            Ok(Err(e)) => Err(AdapterError::Unavailable(format!(
                "Failed to run acpx: {}",
                e
            ))),
            Err(_) => Err(AdapterError::Unavailable("acpx exec timed out".to_string())),
        }
    }

    /// Send prompt to persistent session
    async fn session_prompt(&self, message: &str) -> Result<String, AdapterError> {
        self.ensure_session_dir().await?;

        // Use cwd session (default session name)
        info!(agent = %self.agent_name, "Running acpx prompt with session");

        let mut cmd = Command::new("acpx");
        cmd.arg("--format")
            .arg("text")
            .arg(&self.agent_name)
            .arg("prompt")
            .arg(message)
            .current_dir(&self.session_dir)
            .envs(&self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let timeout = std::time::Duration::from_millis(self.timeout_ms);
        let child = cmd
            .spawn()
            .map_err(|e| AdapterError::Unavailable(format!("Failed to spawn acpx: {}", e)))?;

        let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    // Session might not exist — try creating it
                    if stderr.contains("session") || stderr.contains("not found") {
                        info!("Session not found, creating...");
                        self.ensure_session("cwd").await?;
                        // Retry
                        return self.exec_prompt(message).await;
                    }
                    return Err(AdapterError::Protocol(format!(
                        "acpx prompt failed: {}",
                        stderr
                    )));
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(Self::strip_acpx_noise(&stdout))
            }
            Ok(Err(e)) => Err(AdapterError::Unavailable(format!(
                "Failed to run acpx: {}",
                e
            ))),
            Err(_) => Err(AdapterError::Unavailable(
                "acpx prompt timed out".to_string(),
            )),
        }
    }
}

#[async_trait]
impl AgentAdapter for AcpxAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let message = if let Some(sender) = ctx.sender {
            format!("[From: {}] {}", sender, ctx.message)
        } else {
            ctx.message.to_string()
        };

        // Try session mode first, fall back to exec
        match self.session_prompt(&message).await {
            Ok(response) => Ok(response),
            Err(AdapterError::Protocol(_)) => {
                // Session error — try one-shot exec
                self.exec_prompt(&message).await
            }
            Err(e) => Err(e),
        }
    }

    fn kind(&self) -> &'static str {
        "acpx"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_is_acpx() {
        let adapter = AcpxAdapter::new("claude".to_string(), None, None, None);
        assert_eq!(adapter.kind(), "acpx");
    }

    #[test]
    fn test_strip_acpx_noise_removes_client_lines() {
        let raw = "[client] setup complete\nhello world";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "hello world");
    }

    #[test]
    fn test_strip_acpx_noise_removes_thinking() {
        let raw = "[thinking] Analyzing request...\nHere is the answer.";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "Here is the answer.");
    }

    #[test]
    fn test_strip_acpx_noise_removes_tool() {
        let raw = "[tool] calling read_file\n[tool] result OK\nDone.";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "Done.");
    }

    #[test]
    fn test_strip_acpx_noise_removes_done_marker() {
        let raw = "I wrote the code.\n[done]";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "I wrote the code.");
    }

    #[test]
    fn test_strip_acpx_noise_removes_error_marker() {
        let raw = "something went wrong\n[error]";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "something went wrong");
    }

    #[test]
    fn test_strip_acpx_noise_strips_empty_lines() {
        let raw = "line one\n\n\nline two\n";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "line one\nline two");
    }

    #[test]
    fn test_strip_acpx_noise_preserves_all_content() {
        let raw = "response line 1\nresponse line 2";
        assert_eq!(
            AcpxAdapter::strip_acpx_noise(raw),
            "response line 1\nresponse line 2"
        );
    }

    #[test]
    fn test_strip_acpx_noise_mixed() {
        let raw = "[client] ready\n[thinking] pondering\nanswer\n[tool] read\n[done]";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "answer");
    }

    #[test]
    fn test_strip_acpx_noise_empty_input() {
        assert_eq!(AcpxAdapter::strip_acpx_noise(""), "");
    }

    #[test]
    fn test_strip_acpx_noise_all_protocol_lines() {
        let raw = "[client] x\n[tool] y\n[thinking] z\n[done]";
        assert_eq!(AcpxAdapter::strip_acpx_noise(raw), "");
    }

    #[test]
    fn test_new_defaults() {
        let adapter = AcpxAdapter::new("claude".to_string(), None, None, None);
        assert_eq!(adapter.agent_name, "claude");
        assert_eq!(adapter.timeout_ms, 300_000);
        assert!(adapter._args.is_empty());
        assert!(adapter.env.is_empty());
    }

    #[test]
    fn test_new_with_args_and_env() {
        let mut env = HashMap::new();
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-test".to_string());
        let adapter = AcpxAdapter::new(
            "claude".to_string(),
            Some(vec!["--model".to_string(), "sonnet".to_string()]),
            Some(env.clone()),
            Some(60_000),
        );
        assert_eq!(adapter.agent_name, "claude");
        assert_eq!(adapter.timeout_ms, 60_000);
        assert_eq!(adapter._args, vec!["--model", "sonnet"]);
        assert_eq!(adapter.env, env);
    }
}
