//! Artifact CLI adapter.
//!
//! Runs a local command with a per-run artifact directory, writes the user task
//! on stdin, validates files created under that directory, and returns an
//! outbound message with a text fallback plus discovered artifacts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::artifacts::{
    collect_run_artifacts, create_run_dir, DEFAULT_MAX_ARTIFACTS, DEFAULT_MAX_ARTIFACT_BYTES,
};
use crate::messages::{OutboundAttachment, OutboundMessage};

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const ARTIFACT_ROOT_NAME: &str = "calciforge-artifacts";
const ARTIFACT_DIR_PLACEHOLDER: &str = "{artifact_dir}";
const MESSAGE_PLACEHOLDER: &str = "{message}";
const MODEL_PLACEHOLDER: &str = "{model}";
const STDIN_TASK_PROMPT: &str = "Read the user task from stdin.";

pub struct ArtifactCliAdapter {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    model: Option<String>,
    timeout: Duration,
    artifact_root: PathBuf,
    max_artifact_bytes: u64,
}

impl ArtifactCliAdapter {
    pub fn new(
        command: String,
        args: Option<Vec<String>>,
        env: HashMap<String, String>,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let artifact_root = std::env::temp_dir().join("calciforge-artifacts");
        Self::with_artifact_root(
            command,
            args,
            env,
            model,
            timeout_ms,
            artifact_root,
            DEFAULT_MAX_ARTIFACT_BYTES,
        )
    }

    pub fn with_artifact_root(
        command: String,
        args: Option<Vec<String>>,
        env: HashMap<String, String>,
        model: Option<String>,
        timeout_ms: Option<u64>,
        artifact_root: PathBuf,
        max_artifact_bytes: u64,
    ) -> Self {
        Self {
            command,
            args: args.unwrap_or_default(),
            env,
            model,
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            artifact_root,
            max_artifact_bytes,
        }
    }

    fn build_args(&self, artifact_dir: &Path, model_override: Option<&str>) -> Vec<String> {
        let model = model_override.or(self.model.as_deref()).unwrap_or("");
        let artifact_dir = artifact_dir.display().to_string();
        self.args
            .iter()
            .map(|arg| {
                arg.replace(ARTIFACT_DIR_PLACEHOLDER, &artifact_dir)
                    .replace(MODEL_PLACEHOLDER, model)
                    .replace(MESSAGE_PLACEHOLDER, STDIN_TASK_PROMPT)
            })
            .collect()
    }

    fn build_env(
        &self,
        artifact_dir: &Path,
        model_override: Option<&str>,
    ) -> HashMap<String, String> {
        let model = model_override.or(self.model.as_deref()).unwrap_or("");
        let artifact_dir = artifact_dir.display().to_string();
        let mut env: HashMap<String, String> = self
            .env
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .replace(ARTIFACT_DIR_PLACEHOLDER, &artifact_dir)
                        .replace(MODEL_PLACEHOLDER, model),
                )
            })
            .collect();
        env.insert("CALCIFORGE_ARTIFACT_DIR".to_string(), artifact_dir);
        if !model.is_empty() {
            env.insert("CALCIFORGE_MODEL".to_string(), model.to_string());
            env.insert("CALCIFORGE_MODEL_OVERRIDE".to_string(), model.to_string());
        }
        env
    }

    fn run_artifact_dir(&self) -> Result<PathBuf, AdapterError> {
        if self.artifact_root == crate::artifacts::artifact_root(ARTIFACT_ROOT_NAME) {
            return create_run_dir(ARTIFACT_ROOT_NAME).map_err(AdapterError::Unavailable);
        }

        let run_dir = self.artifact_root.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&run_dir).map_err(|e| {
            AdapterError::Unavailable(format!(
                "failed to create artifact directory {}: {e}",
                run_dir.display()
            ))
        })?;
        Ok(run_dir)
    }

    fn stderr_preview(stderr: &[u8]) -> String {
        const MAX_PREVIEW_CHARS: usize = 512;

        let raw = String::from_utf8_lossy(stderr);
        raw.chars()
            .take(MAX_PREVIEW_CHARS)
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>()
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

    fn discover_artifacts(
        &self,
        artifact_dir: &Path,
    ) -> Result<Vec<OutboundAttachment>, AdapterError> {
        collect_run_artifacts(artifact_dir, self.max_artifact_bytes, DEFAULT_MAX_ARTIFACTS)
            .map_err(AdapterError::Protocol)
    }
}

#[async_trait]
impl AgentAdapter for ArtifactCliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let message = self
            .dispatch_message_with_context(DispatchContext::message_only(msg))
            .await?;
        Ok(message.render_text_fallback())
    }

    async fn dispatch_message_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<OutboundMessage, AdapterError> {
        let artifact_dir = self.run_artifact_dir()?;

        let args = self.build_args(&artifact_dir, ctx.model_override);
        let env = self.build_env(&artifact_dir, ctx.model_override);
        info!(
            command = %self.command,
            arg_count = args.len(),
            artifact_dir = %artifact_dir.display(),
            "artifact-cli dispatch"
        );
        debug!(
            message_bytes = ctx.message.len(),
            "artifact-cli outbound message"
        );

        let output = tokio::time::timeout(self.timeout, async {
            let mut child = Command::new(&self.command)
                .args(&args)
                .envs(&env)
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

        if !output.stderr.is_empty() {
            debug!(
                command = %self.command,
                stderr_bytes = output.stderr.len(),
                stderr_preview = %Self::stderr_preview(&output.stderr),
                "artifact-cli stderr"
            );
        }

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            warn!(
                command = %self.command,
                code,
                stderr_preview = %Self::stderr_preview(&output.stderr),
                "artifact-cli exited unsuccessfully"
            );
            return Err(AdapterError::Protocol(format!(
                "artifact CLI exited with code {code}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let attachments = self.discover_artifacts(&artifact_dir)?;
        if stdout.is_empty() && attachments.is_empty() {
            return Err(AdapterError::Protocol(
                "artifact CLI produced no output or artifacts".to_string(),
            ));
        }

        Ok(OutboundMessage {
            text: if stdout.is_empty() {
                None
            } else {
                Some(stdout)
            },
            attachments,
        })
    }

    fn kind(&self) -> &'static str {
        "artifact-cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn make_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-agent.sh");
        let mut file = std::fs::File::create(&path).expect("create script");
        writeln!(file, "#!/bin/sh").expect("write shebang");
        writeln!(file, "{body}").expect("write body");
        file.sync_all().expect("sync script");
        let mut perms = file.metadata().expect("script metadata").permissions();
        drop(file);
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
        path
    }

    fn script_adapter(
        script: &Path,
        args: Option<Vec<String>>,
        artifact_root: PathBuf,
        max_artifact_bytes: u64,
    ) -> ArtifactCliAdapter {
        let mut shell_args = vec![script.display().to_string()];
        shell_args.extend(args.unwrap_or_default());
        ArtifactCliAdapter::with_artifact_root(
            "/bin/sh".to_string(),
            Some(shell_args),
            HashMap::new(),
            None,
            Some(5000),
            artifact_root,
            max_artifact_bytes,
        )
    }

    #[tokio::test]
    async fn dispatch_captures_artifact_and_stdout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = make_script(
            temp.path(),
            "cat >/dev/null\nprintf '\\211PNG\\r\\n\\032\\n' > \"$CALCIFORGE_ARTIFACT_DIR/out.png\"\necho generated image",
        );
        let adapter = script_adapter(
            &script,
            None,
            temp.path().join("artifacts"),
            DEFAULT_MAX_ARTIFACT_BYTES,
        );

        let response = adapter
            .dispatch_message_with_context(DispatchContext::message_only("draw this"))
            .await
            .expect("dispatch should succeed");

        assert_eq!(response.text.as_deref(), Some("generated image"));
        assert_eq!(response.attachments.len(), 1);
        assert_eq!(response.attachments[0].mime_type, "image/png");
        assert!(response.render_text_fallback().contains("Attachments:"));
    }

    #[tokio::test]
    async fn dispatch_sends_prompt_on_stdin_not_argv_placeholder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = make_script(
            temp.path(),
            "read task\nprintf '%s' \"$1\" > \"$CALCIFORGE_ARTIFACT_DIR/arg.txt\"\nprintf '%s' \"$task\"",
        );
        let adapter = script_adapter(
            &script,
            Some(vec![MESSAGE_PLACEHOLDER.to_string()]),
            temp.path().join("artifacts"),
            DEFAULT_MAX_ARTIFACT_BYTES,
        );

        let response = adapter
            .dispatch_message_with_context(DispatchContext::message_only("secret task text"))
            .await
            .expect("dispatch should succeed");

        assert_eq!(response.text.as_deref(), Some("secret task text"));
        let arg_artifact = response
            .attachments
            .iter()
            .find(|a| a.path.ends_with("arg.txt"))
            .expect("arg artifact should exist");
        let arg_text = std::fs::read_to_string(&arg_artifact.path).expect("read arg artifact");
        assert_eq!(arg_text, STDIN_TASK_PROMPT);
    }

    #[tokio::test]
    async fn dispatch_rejects_oversized_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = make_script(
            temp.path(),
            "cat >/dev/null\nprintf 'too large' > \"$CALCIFORGE_ARTIFACT_DIR/out.txt\"\necho done",
        );
        let adapter = script_adapter(&script, None, temp.path().join("artifacts"), 4);

        let err = adapter
            .dispatch_message_with_context(DispatchContext::message_only("make file"))
            .await
            .expect_err("oversized artifact should fail");
        match err {
            AdapterError::Protocol(msg) => assert!(msg.contains("exceeds")),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_rejects_too_many_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = make_script(
            temp.path(),
            "cat >/dev/null\ni=0\nwhile [ \"$i\" -le 16 ]; do printf x > \"$CALCIFORGE_ARTIFACT_DIR/$i.txt\"; i=$((i + 1)); done\necho done",
        );
        let adapter = script_adapter(
            &script,
            None,
            temp.path().join("artifacts"),
            DEFAULT_MAX_ARTIFACT_BYTES,
        );

        let err = adapter
            .dispatch_message_with_context(DispatchContext::message_only("make files"))
            .await
            .expect_err("too many artifacts should fail");
        match err {
            AdapterError::Protocol(msg) => assert!(msg.contains("artifact count exceeds")),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }
}
