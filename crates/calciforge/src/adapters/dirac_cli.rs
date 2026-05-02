//! Dirac CLI adapter.
//!
//! This adapter dispatches messages through `dirac` in scripted mode.
//! Default invocation uses `--yolo --json` with a fixed argv prompt and writes
//! the user task on stdin, avoiding prompt leakage through process listings.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;
const MESSAGE_PLACEHOLDER: &str = "{message}";
const STDIN_TASK_PROMPT: &str = "Read the user task from stdin and execute it.";

pub struct DiracCliAdapter {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl DiracCliAdapter {
    pub fn new(
        command: Option<String>,
        args: Option<Vec<String>>,
        model: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            command: command.unwrap_or_else(|| "dirac".to_string()),
            args: args.unwrap_or_else(default_dirac_args),
            model,
            env: env.unwrap_or_default(),
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
        }
    }

    fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = self
            .args
            .iter()
            .map(|arg| arg.replace(MESSAGE_PLACEHOLDER, STDIN_TASK_PROMPT))
            .collect();

        let has_placeholder = self
            .args
            .iter()
            .any(|arg| arg.contains(MESSAGE_PLACEHOLDER));
        if !has_placeholder {
            args.push(STDIN_TASK_PROMPT.to_string());
        }

        if let Some(model) = &self.model {
            let has_model_flag = args
                .iter()
                .any(|arg| arg == "--model" || arg == "-m" || arg.starts_with("--model="));
            if !has_model_flag {
                args.insert(0, model.clone());
                args.insert(0, "--model".to_string());
            }
        }

        args
    }

    fn stderr_preview(stderr: &[u8]) -> String {
        const MAX_PREVIEW_CHARS: usize = 512;

        let raw = String::from_utf8_lossy(stderr);
        let mut preview = raw.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
        if raw.chars().count() > MAX_PREVIEW_CHARS {
            preview.push_str("...");
        }

        let sanitized = preview
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>();

        sanitized
            .split_whitespace()
            .map(|word| {
                let lower = word.to_ascii_lowercase();
                if lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("password")
                    || lower.contains("apikey")
                    || lower.contains("api_key")
                    || lower.contains("authorization")
                {
                    "[redacted]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn parse_json_response(stdout: &str) -> Option<String> {
        let mut last_completion: Option<String> = None;
        let mut last_plain_say: Option<String> = None;

        for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let event_type = parsed.get("type").and_then(|v| v.as_str());
            let say_kind = parsed.get("say").and_then(|v| v.as_str());
            let text = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let partial = parsed
                .get("partial")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if event_type != Some("say") || partial || text.is_empty() {
                continue;
            }

            match say_kind {
                Some("completion_result") => last_completion = Some(text.to_string()),
                Some("text") | Some("message") => last_plain_say = Some(text.to_string()),
                _ => {}
            }
        }

        last_completion.or(last_plain_say)
    }

    fn parse_plain_response(stdout: &str) -> Option<String> {
        stdout
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(ToString::to_string)
    }
}

fn default_dirac_args() -> Vec<String> {
    vec!["--yolo".to_string(), "--json".to_string()]
}

#[async_trait]
impl AgentAdapter for DiracCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let args = self.build_args();
        info!(
            command = %self.command,
            arg_count = args.len(),
            "dirac-cli dispatch"
        );
        debug!(message_bytes = msg.len(), "dirac-cli outbound message");

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
                stdin.write_all(msg.as_bytes()).await?;
                let _ = stdin.shutdown().await;
            }

            child.wait_with_output().await
        })
        .await
        .map_err(|_| AdapterError::Timeout)?
        .map_err(|e| AdapterError::Unavailable(format!("failed to run {}: {}", self.command, e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        if !output.stderr.is_empty() {
            debug!(
                command = %self.command,
                stderr_bytes = output.stderr.len(),
                stderr_preview = %Self::stderr_preview(&output.stderr),
                "dirac-cli stderr"
            );
        }

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            warn!(
                command = %self.command,
                code,
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
                stderr_preview = %Self::stderr_preview(&output.stderr),
                "dirac-cli exited unsuccessfully"
            );
            return Err(AdapterError::Protocol(format!(
                "dirac exited with code {code}"
            )));
        }

        if let Some(reply) =
            Self::parse_json_response(&stdout).or_else(|| Self::parse_plain_response(&stdout))
        {
            return Ok(reply);
        }

        Err(AdapterError::Protocol(
            "dirac produced no assistant response".to_string(),
        ))
    }

    fn kind(&self) -> &'static str {
        "dirac-cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_append_prompt_when_no_placeholder() {
        let a = DiracCliAdapter::new(None, None, None, None, None);
        let args = a.build_args();
        assert!(args.contains(&"--yolo".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert_eq!(args.last().map(String::as_str), Some(STDIN_TASK_PROMPT));
    }

    #[test]
    fn placeholder_replaces_with_safe_stdin_prompt_in_custom_args() {
        let a = DiracCliAdapter::new(
            None,
            Some(vec!["task".to_string(), MESSAGE_PLACEHOLDER.to_string()]),
            None,
            None,
            None,
        );
        let args = a.build_args();
        assert_eq!(
            args,
            vec!["task".to_string(), STDIN_TASK_PROMPT.to_string()]
        );
    }

    #[test]
    fn model_flag_is_injected_when_missing() {
        let a = DiracCliAdapter::new(None, None, Some("gpt-5".to_string()), None, None);
        let args = a.build_args();
        assert_eq!(args[0], "--model");
        assert_eq!(args[1], "gpt-5");
    }

    #[test]
    fn parse_json_prefers_last_non_partial_say() {
        let stdout = r#"{"type":"say","say":"api_req_started","text":"internal request","partial":false}
{"type":"say","say":"text","text":"working","partial":true}
{"type":"say","say":"completion_result","text":"final answer","partial":false}
{"type":"ask","text":"anything else?","partial":false}"#;
        let parsed = DiracCliAdapter::parse_json_response(stdout);
        assert_eq!(parsed.as_deref(), Some("final answer"));
    }

    #[test]
    fn parse_json_ignores_internal_request_events() {
        let stdout = r#"{"type":"say","say":"api_req_started","text":"internal request","partial":false}
{"type":"say","say":"tool","text":"tool output","partial":false}"#;
        assert_eq!(DiracCliAdapter::parse_json_response(stdout), None);
    }

    #[tokio::test]
    async fn dispatch_parses_json_stream() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-dirac");
        std::fs::write(
            &script_path,
            br#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"say","text":"step","partial":true}'
printf '%s\n' '{"type":"say","say":"completion_result","text":"done","partial":false}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let adapter = DiracCliAdapter::new(
            Some(script_path.to_string_lossy().to_string()),
            Some(vec![MESSAGE_PLACEHOLDER.to_string()]),
            None,
            None,
            Some(5_000),
        );

        let response = adapter.dispatch("hello").await.unwrap();
        assert_eq!(response, "done");
    }
}
