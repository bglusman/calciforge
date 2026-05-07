//! Claude Code CLI adapter.
//!
//! Runs Claude Code in non-interactive print mode. When Calciforge has a
//! selected downstream session, the adapter passes it as `--session-id`.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;

pub struct ClaudeCliAdapter {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl ClaudeCliAdapter {
    pub fn new(
        command: Option<String>,
        args: Option<Vec<String>>,
        model: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            command: command.unwrap_or_else(|| "claude".to_string()),
            args: args.unwrap_or_else(default_claude_args),
            model,
            env: env.unwrap_or_default(),
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
        }
    }

    fn has_arg(args: &[String], long: &str, short: &str) -> bool {
        args.iter()
            .any(|arg| arg == long || arg == short || arg.starts_with(&format!("{long}=")))
    }

    fn build_args(&self, model_override: Option<&str>, session: Option<&str>) -> Vec<String> {
        let mut args = self.args.clone();
        if let Some(model) = model_override.or(self.model.as_deref()) {
            if !Self::has_arg(&args, "--model", "") {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
        }
        if let Some(session) = session.filter(|session| !session.trim().is_empty()) {
            if !Self::has_arg(&args, "--session-id", "") {
                args.push("--session-id".to_string());
                args.push(session.to_string());
            }
        }
        args
    }
}

fn default_claude_args() -> Vec<String> {
    vec![
        "--print".to_string(),
        "--input-format".to_string(),
        "text".to_string(),
        "--output-format".to_string(),
        "text".to_string(),
    ]
}

#[async_trait]
impl AgentAdapter for ClaudeCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let args = self.build_args(ctx.model_override, ctx.session);
        info!(command = %self.command, args = ?args, "claude-cli dispatch");
        debug!(
            message_bytes = ctx.message.len(),
            "claude-cli outbound message"
        );

        let output = tokio::time::timeout(self.timeout, async {
            let mut child = Command::new(&self.command)
                .args(&args)
                .envs(&self.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(ctx.message.as_bytes()).await?;
                let _ = stdin.shutdown().await;
            }

            child.wait_with_output().await
        })
        .await
        .map_err(|_| AdapterError::Timeout)?
        .map_err(|e| AdapterError::Unavailable(format!("failed to run {}: {}", self.command, e)))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            warn!(stderr = %stderr.trim(), "claude-cli stderr");
        }
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            return Err(AdapterError::Protocol(format!(
                "claude exited with code {code}: {}",
                stderr.trim()
            )));
        }

        let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if response.is_empty() {
            return Err(AdapterError::Protocol(
                "claude produced no assistant response".to_string(),
            ));
        }
        Ok(response)
    }

    fn kind(&self) -> &'static str {
        "claude-cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_model_flags_are_added_when_missing() {
        let adapter = ClaudeCliAdapter::new(None, None, Some("sonnet".to_string()), None, None);
        let args = adapter.build_args(None, Some("550e8400-e29b-41d4-a716-446655440000"));

        assert!(args.contains(&"--print".to_string()));
        assert_eq!(
            args.windows(2)
                .find(|pair| pair[0] == "--model")
                .map(|pair| pair[1].as_str()),
            Some("sonnet")
        );
        assert_eq!(
            args.windows(2)
                .find(|pair| pair[0] == "--session-id")
                .map(|pair| pair[1].as_str()),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }
}
