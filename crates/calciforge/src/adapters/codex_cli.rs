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

use super::{AdapterError, AgentAdapter, DispatchContext};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputCapture {
    path: PathBuf,
    cleanup: bool,
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

    fn has_stdin_marker(args: &[String]) -> bool {
        args.iter().any(|arg| arg == "-")
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

    fn configured_output_path(args: &[String]) -> Result<Option<PathBuf>, AdapterError> {
        for (index, arg) in args.iter().enumerate() {
            if arg == "--output-last-message" || arg == "-o" {
                let Some(path) = args.get(index + 1) else {
                    return Err(AdapterError::Protocol(format!(
                        "{arg} requires a file path argument"
                    )));
                };
                return Ok(Some(PathBuf::from(path)));
            }
            if let Some(path) = arg.strip_prefix("--output-last-message=") {
                if path.is_empty() {
                    return Err(AdapterError::Protocol(
                        "--output-last-message requires a non-empty file path".to_string(),
                    ));
                }
                return Ok(Some(PathBuf::from(path)));
            }
        }
        Ok(None)
    }

    fn build_args(
        &self,
        message: &str,
        model_override: Option<&str>,
        output_path: &Path,
    ) -> Result<(Vec<String>, Option<String>, OutputCapture), AdapterError> {
        let has_placeholder = Self::has_message_placeholder(&self.args);
        if has_placeholder && Self::has_stdin_marker(&self.args) {
            return Err(AdapterError::Protocol(
                "codex-cli args cannot combine {message} and '-' stdin prompt modes".to_string(),
            ));
        }

        let mut args: Vec<String> = self
            .args
            .iter()
            .map(|arg| arg.replace(MESSAGE_PLACEHOLDER, message))
            .collect();
        let configured_output_path = Self::configured_output_path(&args)?;

        let insert_at = Self::prompt_arg_index(&self.args, has_placeholder).unwrap_or(args.len());
        let mut generated = Vec::new();

        let selected_model = model_override.or(self.model.as_deref());
        if let Some(model) = selected_model {
            if !Self::has_arg(&args, "--model", "-m") {
                generated.push("--model".to_string());
                generated.push(model.to_string());
            }
        }

        if configured_output_path.is_none() {
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

        let cleanup = configured_output_path.is_none();
        let capture = OutputCapture {
            path: configured_output_path.unwrap_or_else(|| output_path.to_path_buf()),
            cleanup,
        };

        Ok((args, stdin_message, capture))
    }
}

fn default_codex_args() -> Vec<String> {
    vec![
        "exec".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--sandbox".to_string(),
        "read-only".to_string(),
        "--ephemeral".to_string(),
        "--skip-git-repo-check".to_string(),
    ]
}

#[async_trait]
impl AgentAdapter for CodexCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        tokio::fs::create_dir_all(&self.output_dir)
            .await
            .map_err(|e| {
                AdapterError::Unavailable(format!("failed to create Codex output dir: {e}"))
            })?;

        let output_path = self.output_path();
        let (args, stdin_message, capture) =
            self.build_args(ctx.message, ctx.model_override, &output_path)?;

        info!(command = %self.command, args = ?args, "codex-cli dispatch");
        debug!(msg = %ctx.message, "codex-cli outbound message");

        let mut cmd = Command::new(&self.command);
        cmd.args(&args)
            .envs(&self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

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
            let _ = stdin.shutdown().await;
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

        let from_file = tokio::fs::read_to_string(&capture.path)
            .await
            .map_err(|e| {
                AdapterError::Protocol(format!(
                    "codex exec did not write final message file {}: {e}",
                    capture.path.display()
                ))
            })?;
        if capture.cleanup {
            let _ = tokio::fs::remove_file(&capture.path).await;
        }

        let response = from_file.trim().to_string();

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

        let (args, stdin_message, capture) =
            adapter.build_args("hello", None, &output_path).unwrap();

        assert!(args.starts_with(&["exec".to_string()]));
        assert!(args.contains(&"--ephemeral".to_string()));
        assert!(!args.contains(&"--ask-for-approval".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.5".to_string()));
        assert!(args.contains(&"--output-last-message".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert_eq!(stdin_message.as_deref(), Some("hello"));
        assert_eq!(capture.path, output_path);
        assert!(capture.cleanup);
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

        let (args, stdin_message, _) = adapter
            .build_args("hello", None, &PathBuf::from("/tmp/out.txt"))
            .unwrap();

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

        let (args, stdin_message, _) = adapter
            .build_args("hello", None, &PathBuf::from("/tmp/out.txt"))
            .unwrap();

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

    #[test]
    fn model_override_takes_precedence_over_configured_model() {
        let adapter = CodexCliAdapter::new(
            None,
            Some(vec!["exec".to_string(), "-".to_string()]),
            Some("gpt-5.5".to_string()),
            None,
            None,
        );

        let (args, _, _) = adapter
            .build_args(
                "hello",
                Some("local-kimi-gpt55"),
                &PathBuf::from("/tmp/out.txt"),
            )
            .unwrap();

        let model_pos = args.iter().position(|arg| arg == "--model").unwrap();
        assert_eq!(
            args.get(model_pos + 1).map(String::as_str),
            Some("local-kimi-gpt55")
        );
        assert!(!args.contains(&"gpt-5.5".to_string()));
    }

    #[test]
    fn placeholder_and_stdin_marker_are_rejected() {
        let adapter = CodexCliAdapter::new(
            None,
            Some(vec![
                "exec".to_string(),
                MESSAGE_PLACEHOLDER.to_string(),
                "-".to_string(),
            ]),
            None,
            None,
            None,
        );

        let err = adapter
            .build_args("hello", None, &PathBuf::from("/tmp/out.txt"))
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(msg) if msg.contains("cannot combine")));
    }

    #[test]
    fn configured_output_path_is_used_without_cleanup() {
        let adapter = CodexCliAdapter::new(
            None,
            Some(vec![
                "exec".to_string(),
                "--output-last-message=/tmp/custom.txt".to_string(),
                "-".to_string(),
            ]),
            None,
            None,
            None,
        );

        let (args, _, capture) = adapter
            .build_args("hello", None, &PathBuf::from("/tmp/generated.txt"))
            .unwrap();

        assert!(!args.contains(&"--output-last-message".to_string()));
        assert_eq!(capture.path, PathBuf::from("/tmp/custom.txt"));
        assert!(!capture.cleanup);
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

    #[tokio::test]
    async fn dispatch_errors_when_final_message_file_is_missing() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-codex-no-output");
        let mut script = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script_path)
            .unwrap();
        writeln!(
            script,
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -)
      cat >/dev/null
      shift
      ;;
    *)
      shift
      ;;
  esac
done
printf 'stdout fallback should not be used\n'
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

        let err = adapter.dispatch("hello").await.unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(msg) if msg.contains("did not write")));
    }
}
