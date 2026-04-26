//! Executable-backed model-gateway provider.
//!
//! This adapts subscription-authenticated local CLIs such as `codex exec` and
//! `claude -p` to Calciforge's OpenAI-compatible gateway. The CLI owns OAuth
//! or subscription credentials; Calciforge owns routing, policy, and response
//! wrapping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::proxy::backend::{BackendError, ModelInfo};
use crate::proxy::gateway::{GatewayBackend, GatewayConfig, GatewayType};
use crate::proxy::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, MessageContent, Usage,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug)]
pub struct ExecGateway {
    config: GatewayConfig,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Duration,
    output_dir: PathBuf,
}

impl ExecGateway {
    pub fn new(
        config: GatewayConfig,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        let timeout_seconds = if config.timeout_seconds == 0 {
            DEFAULT_TIMEOUT_SECONDS
        } else {
            config.timeout_seconds
        };
        Self {
            config,
            command,
            args,
            env,
            timeout: Duration::from_secs(timeout_seconds),
            output_dir: std::env::temp_dir().join("calciforge-exec-gateway"),
        }
    }

    fn output_path(&self) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        self.output_dir
            .join(format!("exec-gateway-last-message-{nanos}.txt"))
    }

    fn render_prompt(req: &ChatCompletionRequest) -> String {
        req.messages
            .iter()
            .filter_map(|msg| {
                let text = msg.content.as_ref().and_then(|c| c.to_text())?;
                Some(format!("{}: {}", msg.role, text))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn build_args(&self, req: &ChatCompletionRequest, output_path: &Path) -> (Vec<String>, bool) {
        let prompt = Self::render_prompt(req);
        let output_file = output_path.to_string_lossy();
        let mut saw_prompt_placeholder = false;
        let mut saw_output_placeholder = false;
        let args = self
            .args
            .iter()
            .map(|arg| {
                if arg.contains("{prompt}") || arg.contains("{message}") {
                    saw_prompt_placeholder = true;
                }
                if arg.contains("{output_file}") {
                    saw_output_placeholder = true;
                }
                arg.replace("{prompt}", &prompt)
                    .replace("{message}", &prompt)
                    .replace("{model}", &req.model)
                    .replace("{output_file}", &output_file)
            })
            .collect::<Vec<_>>();

        let should_write_stdin = !saw_prompt_placeholder || args.iter().any(|arg| arg == "-");
        let _ = saw_output_placeholder;
        (args, should_write_stdin)
    }

    async fn read_response(output_path: &Path, stdout: &[u8]) -> Result<String, BackendError> {
        if output_path.exists() {
            let from_file = tokio::fs::read_to_string(output_path).await.map_err(|e| {
                BackendError::InvalidResponse(format!(
                    "failed reading exec output file {}: {e}",
                    output_path.display()
                ))
            })?;
            let _ = tokio::fs::remove_file(output_path).await;
            let trimmed = from_file.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

        let stdout = String::from_utf8_lossy(stdout).trim().to_string();
        if stdout.is_empty() {
            return Err(BackendError::InvalidResponse(
                "exec provider produced no response".to_string(),
            ));
        }
        Ok(stdout)
    }

    fn wrap_response(req: &ChatCompletionRequest, content: String) -> ChatCompletionResponse {
        let prompt_tokens = Self::render_prompt(req).len() as u32 / 4;
        let completion_tokens = content.len() as u32 / 4;
        ChatCompletionResponse {
            id: format!("chatcmpl-exec-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: req.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(content)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_content: None,
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            system_fingerprint: None,
        }
    }
}

#[async_trait::async_trait]
impl GatewayBackend for ExecGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Direct
    }

    async fn chat_completion(
        &self,
        req: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        tokio::fs::create_dir_all(&self.output_dir)
            .await
            .map_err(|e| BackendError::ExecutionFailed(format!("create output dir: {e}")))?;

        let output_path = self.output_path();
        let (args, should_write_stdin) = self.build_args(&req, &output_path);
        let prompt = Self::render_prompt(&req);

        let mut cmd = Command::new(&self.command);
        cmd.args(&args)
            .envs(&self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if should_write_stdin {
            cmd.stdin(Stdio::piped());
        }

        let mut child = cmd.spawn().map_err(|e| {
            BackendError::NotAvailable(format!("failed to spawn {}: {e}", self.command))
        })?;

        if should_write_stdin {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(prompt.as_bytes()).await.map_err(|e| {
                    BackendError::ExecutionFailed(format!("failed writing exec stdin: {e}"))
                })?;
                let _ = stdin.shutdown().await;
            }
        }

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| BackendError::ExecutionFailed("exec provider timed out".to_string()))?
            .map_err(|e| BackendError::ExecutionFailed(format!("exec provider failed: {e}")))?;

        if !output.status.success() {
            return Err(BackendError::ExecutionFailed(format!(
                "exec provider exited with code {:?}",
                output.status.code()
            )));
        }

        let content = Self::read_response(&output_path, &output.stdout).await?;
        Ok(Self::wrap_response(&req, content))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        Ok(Vec::new())
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    fn request(model: &str, content: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(content.to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn exec_gateway_reads_output_file_when_template_uses_one() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(
            f,
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    -) prompt="$(cat)"; shift ;;
    *) shift ;;
  esac
done
printf 'file:%s\n' "$prompt" > "$out"
"#
        )
        .unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec![
                "--out".to_string(),
                "{output_file}".to_string(),
                "-".to_string(),
            ],
            HashMap::new(),
        );

        let resp = gw
            .chat_completion(request("gpt-5.5", "hello"))
            .await
            .unwrap();
        let text = resp.choices[0]
            .message
            .content
            .as_ref()
            .and_then(|c| c.to_text())
            .unwrap();
        assert_eq!(text, "file:user: hello");
    }

    #[tokio::test]
    async fn exec_gateway_uses_prompt_placeholder_without_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(
            f,
            r#"#!/bin/sh
printf 'stdout:%s:%s\n' "$1" "$2"
"#
        )
        .unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["{model}".to_string(), "{prompt}".to_string()],
            HashMap::new(),
        );

        let resp = gw
            .chat_completion(request("claude/sonnet", "hi"))
            .await
            .unwrap();
        let text = resp.choices[0]
            .message
            .content
            .as_ref()
            .and_then(|c| c.to_text())
            .unwrap();
        assert_eq!(text, "stdout:claude/sonnet:user: hi");
    }
}
