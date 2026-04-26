//! Codex CLI adapter.
//!
//! This adapter dispatches messages through the official `codex exec`
//! command. It is intended for local Codex subscription/API-key auth owned by
//! the user account running Calciforge, without routing through ACPX.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;
const MESSAGE_PLACEHOLDER: &str = "{message}";

/// Adapter for the official OpenAI Codex CLI.
pub struct CodexCliAdapter {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    env: HashMap<String, String>,
    timeout: Duration,
    output_dir: PathBuf,
}

impl CodexCliAdapter {
    /// Create a new Codex CLI adapter.
    pub fn new(
        command: Option<String>,
        args: Option<Vec<String>>,
        model: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            command: command.unwrap_or_else(|| "codex".to_string()),
            args: args.unwrap_or_else(default_codex_args),
            model,
            env: env.unwrap_or_default(),
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            output_dir: std::env::temp_dir().join("calciforge-codex-cli"),
        }
    }

    fn has_arg(args: &[String], long: &str, short: &str) -> bool {
        args.iter()
            .any(|arg| arg == long || arg == short || arg.starts_with(&format!("{long}=")))
    }

    fn has_message_placeholder(args: &[String]) -> bool {
        args.iter().any(|arg| arg.contains(MESSAGE_PLACEHOLDER))
    }

    fn prompt_arg_index(args: &[String], has_placeholder: bool) -> Option<usize> {
        if has_placeholder {
            return args
                .iter()
                .position(|arg| arg.contains(MESSAGE_PLACEHOLDER));
        }
        args.iter().position(|arg| arg == "-")
    }

    fn insert_generated_flags(args: &mut Vec<String>, insert_at: usize, flags: Vec<String>) {
        for (offset, flag) in flags.into_iter().enumerate() {
            args.insert(insert_at + offset, flag);
        }
    }

    fn output_path(&self) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        self.output_dir
            .join(format!("codex-last-message-{nanos}.txt"))
    }

    fn build_args(&self, message: &str, output_path: &Path) -> (Vec<String>, Option<String>) {
        let has_placeholder = Self::has_message_placeholder(&self.args);
        let mut args: Vec<String> = self
            .args
            .iter()
            .map(|arg| arg.replace(MESSAGE_PLACEHOLDER, message))
            .collect();

        let insert_at = Self::prompt_arg_index(&self.args, has_placeholder).unwrap_or(args.len());
        let mut generated = Vec::new();

        if let Some(model) = &self.model {
            if !Self::has_arg(&args, "--model", "-m") {
                generated.push("--model".to_string());
                generated.push(model.clone());
            }
        }

        if !Self::has_arg(&args, "--output-last-message", "-o") {
            generated.push("--output-last-message".to_string());
            generated.push(output_path.to_string_lossy().to_string());
        }

        Self::insert_generated_flags(&mut args, insert_at, generated);

        let stdin_message = if has_placeholder {
            None
        } else {
            if !self.args.iter().any(|arg| arg == "-") {
                args.push("-".to_string());
            }
            Some(message.to_string())
        };

        (args, stdin_message)
    }
}

fn default_codex_args() -> Vec<String> {
    vec![
        "exec".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--sandbox".to_string(),
        "read-only".to_string(),
        "--ask-for-approval".to_string(),
        "never".to_string(),
        "--skip-git-repo-check".to_string(),
    ]
}

#[async_trait]
impl AgentAdapter for CodexCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        tokio::fs::create_dir_all(&self.output_dir)
            .await
            .map_err(|e| {
                AdapterError::Unavailable(format!("failed to create Codex output dir: {e}"))
            })?;

        let output_path = self.output_path();
        let (args, stdin_message) = self.build_args(msg, &output_path);

        info!(command = %self.command, args = ?args, "codex-cli dispatch");
        debug!(msg = %msg, "codex-cli outbound message");

        let mut cmd = Command::new(&self.command);
        cmd.args(&args)
            .envs(&self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if stdin_message.is_some() {
            cmd.stdin(Stdio::piped());
        }

        let mut child = cmd.spawn().map_err(|e| {
            AdapterError::Unavailable(format!("failed to spawn {}: {}", self.command, e))
        })?;

        if let Some(input) = stdin_message {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                AdapterError::Unavailable("failed to open Codex stdin".to_string())
            })?;
            stdin.write_all(input.as_bytes()).await.map_err(|e| {
                AdapterError::Unavailable(format!("failed to write Codex prompt to stdin: {e}"))
            })?;
        }

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| AdapterError::Timeout)?
            .map_err(|e| AdapterError::Unavailable(format!("failed to run Codex: {e}")))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            warn!(stderr = %stderr.trim(), "codex-cli stderr");
        }

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            return Err(AdapterError::Protocol(format!(
                "codex exec exited with code {code}: {}",
                stderr.trim()
            )));
        }

        let from_file = tokio::fs::read_to_string(&output_path)
            .await
            .unwrap_or_default();
        let _ = tokio::fs::remove_file(&output_path).await;

        let response = if from_file.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            from_file.trim().to_string()
        };

        if response.is_empty() {
            return Err(AdapterError::Protocol(
                "codex exec produced no final message".to_string(),
            ));
        }

        Ok(response)
    }

    fn kind(&self) -> &'static str {
        "codex-cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_args_use_stdin_and_capture_last_message() {
        let adapter =
            CodexCliAdapter::new(None, None, Some("gpt-5.5".to_string()), None, Some(1_000));
        let output_path = PathBuf::from("/tmp/out.txt");

        let (args, stdin_message) = adapter.build_args("hello", &output_path);

        assert!(args.starts_with(&["exec".to_string()]));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.5".to_string()));
        assert!(args.contains(&"--output-last-message".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert_eq!(stdin_message.as_deref(), Some("hello"));
    }

    #[test]
    fn explicit_message_placeholder_does_not_append_stdin_marker() {
        let adapter = CodexCliAdapter::new(
            Some("/opt/codex".to_string()),
            Some(vec![
                "exec".to_string(),
                "--model".to_string(),
                "configured".to_string(),
                MESSAGE_PLACEHOLDER.to_string(),
            ]),
            Some("ignored".to_string()),
            None,
            None,
        );

        let (args, stdin_message) = adapter.build_args("hello", &PathBuf::from("/tmp/out.txt"));

        assert_eq!(args[0], "exec");
        assert_eq!(args.last().map(String::as_str), Some("hello"));
        assert!(args.contains(&"configured".to_string()));
        assert!(!args.contains(&"ignored".to_string()));
        assert!(args.contains(&"hello".to_string()));
        assert!(stdin_message.is_none());
    }

    #[test]
    fn generated_flags_are_inserted_before_existing_stdin_marker() {
        let adapter = CodexCliAdapter::new(
            None,
            Some(vec!["exec".to_string(), "-".to_string()]),
            Some("gpt-5.5".to_string()),
            None,
            None,
        );

        let (args, stdin_message) = adapter.build_args("hello", &PathBuf::from("/tmp/out.txt"));

        let stdin_pos = args.iter().position(|arg| arg == "-").unwrap();
        let output_pos = args
            .iter()
            .position(|arg| arg == "--output-last-message")
            .unwrap();
        let model_pos = args.iter().position(|arg| arg == "--model").unwrap();
        assert!(model_pos < stdin_pos);
        assert!(output_pos < stdin_pos);
        assert_eq!(args.iter().filter(|arg| arg.as_str() == "-").count(), 1);
        assert_eq!(stdin_message.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn dispatch_reads_final_message_file_from_codex_like_cli() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-codex");
        let mut script = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script_path)
            .unwrap();
        writeln!(
            script,
            r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o|--output-last-message)
      out="$2"
      shift 2
      ;;
    -)
      prompt="$(cat)"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
printf 'final:%s\n' "$prompt" > "$out"
printf 'event noise\n'
"#
        )
        .unwrap();
        script.sync_all().unwrap();
        drop(script);

        let adapter = CodexCliAdapter::new(
            Some(script_path.to_string_lossy().to_string()),
            Some(vec!["exec".to_string(), "-".to_string()]),
            None,
            None,
            Some(5_000),
        );

        let response = adapter.dispatch("hello").await.unwrap();
        assert_eq!(response, "final:hello");
    }
}
