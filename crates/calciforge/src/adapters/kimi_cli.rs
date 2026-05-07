//! Kimi Code CLI adapter.
//!
//! Runs `kimi` in non-interactive print mode. Calciforge passes prompts on
//! stdin by default, and forwards selected downstream sessions with `--session`.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;
const MESSAGE_PLACEHOLDER: &str = "{message}";
const MODEL_PLACEHOLDER: &str = "{model}";
const SESSION_PLACEHOLDER: &str = "{session}";
const SESSION_UUID_PLACEHOLDER: &str = "{session_uuid}";

pub struct KimiCliAdapter {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl KimiCliAdapter {
    pub fn new(
        command: Option<String>,
        args: Option<Vec<String>>,
        model: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            command: command.unwrap_or_else(|| "kimi".to_string()),
            args: args.unwrap_or_else(default_kimi_args),
            model,
            env: env.unwrap_or_default(),
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
        }
    }

    fn has_arg(args: &[String], long: &str, short: &str) -> bool {
        args.iter().any(|arg| {
            arg == long
                || (!short.is_empty() && arg == short)
                || arg.starts_with(&format!("{long}="))
        })
    }

    fn has_message_placeholder(args: &[String]) -> bool {
        args.iter().any(|arg| arg.contains(MESSAGE_PLACEHOLDER))
    }

    fn selected_model<'a>(&'a self, model_override: Option<&'a str>) -> Option<&'a str> {
        model_override.or(self.model.as_deref())
    }

    fn build_args(
        &self,
        message: &str,
        model_override: Option<&str>,
        session: Option<&str>,
    ) -> (Vec<String>, Option<String>) {
        let model = self.selected_model(model_override).unwrap_or("");
        let session = session.unwrap_or("");
        let has_message_placeholder = Self::has_message_placeholder(&self.args);
        let mut args: Vec<String> = self
            .args
            .iter()
            .map(|arg| {
                arg.replace(MESSAGE_PLACEHOLDER, message)
                    .replace(MODEL_PLACEHOLDER, model)
                    .replace(SESSION_PLACEHOLDER, session)
                    .replace(SESSION_UUID_PLACEHOLDER, session)
            })
            .collect();

        if !model.is_empty() && !Self::has_arg(&args, "--model", "-m") {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if !session.is_empty() && !Self::has_arg(&args, "--session", "-S") {
            args.push("--session".to_string());
            args.push(session.to_string());
        }

        let stdin_message = if has_message_placeholder {
            None
        } else {
            Some(message.to_string())
        };
        (args, stdin_message)
    }
}

fn default_kimi_args() -> Vec<String> {
    vec![
        "--quiet".to_string(),
        "--input-format".to_string(),
        "text".to_string(),
        "--no-thinking".to_string(),
    ]
}

#[async_trait]
impl AgentAdapter for KimiCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let (args, stdin_message) = self.build_args(ctx.message, ctx.model_override, ctx.session);
        info!(command = %self.command, args = ?args, "kimi-cli dispatch");
        debug!(
            message_bytes = ctx.message.len(),
            "kimi-cli outbound message"
        );

        let output = tokio::time::timeout(self.timeout, async {
            let mut child = Command::new(&self.command)
                .args(&args)
                .envs(&self.env)
                .stdin(if stdin_message.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;

            if let Some(input) = stdin_message {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(input.as_bytes()).await?;
                    let _ = stdin.shutdown().await;
                }
            }

            child.wait_with_output().await
        })
        .await
        .map_err(|_| AdapterError::Timeout)?
        .map_err(|e| AdapterError::Unavailable(format!("failed to run {}: {}", self.command, e)))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            warn!(stderr = %stderr.trim(), "kimi-cli stderr");
        }
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            return Err(AdapterError::Protocol(format!(
                "kimi exited with code {code}: {}",
                stderr.trim()
            )));
        }

        let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if response.is_empty() {
            return Err(AdapterError::Protocol(
                "kimi produced no assistant response".to_string(),
            ));
        }
        Ok(response)
    }

    fn kind(&self) -> &'static str {
        "kimi-cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_stdin_print_mode_with_thinking_disabled() {
        let adapter = KimiCliAdapter::new(None, None, Some("kimi-k2.6".to_string()), None, None);
        let (args, stdin) = adapter.build_args("hello", None, Some("session-123"));

        assert!(args.contains(&"--quiet".to_string()));
        assert!(args.contains(&"--input-format".to_string()));
        assert!(args.contains(&"--no-thinking".to_string()));
        assert_eq!(stdin.as_deref(), Some("hello"));
        assert_eq!(
            args.windows(2)
                .find(|pair| pair[0] == "--model")
                .map(|pair| pair[1].as_str()),
            Some("kimi-k2.6")
        );
        assert_eq!(
            args.windows(2)
                .find(|pair| pair[0] == "--session")
                .map(|pair| pair[1].as_str()),
            Some("session-123")
        );
    }

    #[test]
    fn custom_args_can_enable_thinking_and_use_placeholders() {
        let adapter = KimiCliAdapter::new(
            None,
            Some(vec![
                "--quiet".to_string(),
                "--thinking".to_string(),
                "--model".to_string(),
                "{model}".to_string(),
                "--session".to_string(),
                "{session_uuid}".to_string(),
                "--prompt".to_string(),
                "{message}".to_string(),
            ]),
            Some("kimi-thinking".to_string()),
            None,
            None,
        );

        let (args, stdin) = adapter.build_args("hello", None, Some("abc"));
        assert!(stdin.is_none());
        assert_eq!(
            args,
            vec![
                "--quiet",
                "--thinking",
                "--model",
                "kimi-thinking",
                "--session",
                "abc",
                "--prompt",
                "hello"
            ]
        );
    }
}
