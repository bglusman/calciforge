//! Executable-backed model-gateway provider.
//!
//! This adapts subscription-authenticated local CLIs such as `codex exec` and
//! `claude -p` to Calciforge's OpenAI-compatible gateway. The CLI owns OAuth
//! or subscription credentials; Calciforge owns routing, policy, and response
//! wrapping.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

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
        }
    }

    fn prepare_output_file(&self) -> Result<tempfile::NamedTempFile, BackendError> {
        tempfile::Builder::new()
            .prefix("exec-gateway-")
            .suffix(".txt")
            .tempfile()
            .map_err(|e| BackendError::ExecutionFailed(format!("create exec output file: {e}")))
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
        let output_file = output_path.to_string_lossy();
        let args = self
            .args
            .iter()
            .map(|arg| {
                arg.replace("{prompt}", "")
                    .replace("{message}", "")
                    .replace("{model}", &req.model)
                    .replace("{output_file}", &output_file)
            })
            .collect::<Vec<_>>();

        let should_write_stdin = true;
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

    fn stderr_preview(stderr: &[u8]) -> String {
        const MAX_PREVIEW_BYTES: usize = 512;

        let raw = String::from_utf8_lossy(stderr);
        let mut preview = raw.chars().take(MAX_PREVIEW_BYTES).collect::<String>();
        if raw.chars().count() > MAX_PREVIEW_BYTES {
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

    fn requests_tool_use(req: &ChatCompletionRequest) -> bool {
        if req.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            return true;
        }

        match req.tool_choice.as_ref() {
            None => {}
            Some(crate::proxy::openai::ToolChoice::Mode(mode))
                if mode.trim().eq_ignore_ascii_case("none") => {}
            Some(_) => return true,
        }

        req.messages.iter().any(|msg| {
            msg.role == "tool"
                || msg.role == "function"
                || msg
                    .tool_calls
                    .as_ref()
                    .is_some_and(|tool_calls| !tool_calls.is_empty())
                || msg.tool_call_id.is_some()
        })
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
        if Self::requests_tool_use(&req) {
            return Err(BackendError::NotAvailable(
                "exec providers are text-only and cannot broker tool calls or tool-call history; use a tool-capable HTTP/ACP provider or agent adapter"
                    .to_string(),
            ));
        }

        let output_file = self.prepare_output_file()?;
        let output_path = output_file.path().to_path_buf();
        let result = async {
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
                warn!(
                    command = %self.command,
                    code = ?output.status.code(),
                    stdout_bytes = output.stdout.len(),
                    stderr_bytes = output.stderr.len(),
                    stderr_preview = %Self::stderr_preview(&output.stderr),
                    "exec provider exited unsuccessfully"
                );
                return Err(BackendError::ExecutionFailed(format!(
                    "exec provider exited with code {:?}",
                    output.status.code()
                )));
            }

            let content = match Self::read_response(&output_path, &output.stdout).await {
                Ok(content) => content,
                Err(e) => {
                    debug!(
                        command = %self.command,
                        stdout_bytes = output.stdout.len(),
                        stderr_bytes = output.stderr.len(),
                        stderr_preview = %Self::stderr_preview(&output.stderr),
                        "exec provider produced invalid response"
                    );
                    return Err(e);
                }
            };
            Ok(Self::wrap_response(&req, content))
        }
        .await;

        if let Err(e) = output_file.close() {
            warn!(path = %output_path.display(), error = %e, "failed to remove exec output file");
        }

        result
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
    async fn exec_gateway_uses_prompt_placeholder_via_stdin() {
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
prompt="$(cat)"
printf 'stdout:%s:%s\n' "$1" "$prompt"
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

    #[tokio::test]
    async fn exec_gateway_allows_explicit_no_tool_choice() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(f, "#!/bin/sh\ncat\n").unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["-".to_string()],
            HashMap::new(),
        );

        let mut req = request("kimi-cli", "plain text");
        req.tool_choice = Some(crate::proxy::openai::ToolChoice::Mode("none".to_string()));

        let resp = gw.chat_completion(req).await.unwrap();
        let text = resp.choices[0]
            .message
            .content
            .as_ref()
            .and_then(|c| c.to_text())
            .unwrap();
        assert_eq!(text, "user: plain text");
    }

    #[tokio::test]
    async fn exec_gateway_rejects_tool_choice_auto_without_tool_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(f, "#!/bin/sh\ncat\n").unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["-".to_string()],
            HashMap::new(),
        );

        let mut req = request("kimi-cli", "maybe use a tool");
        req.tool_choice = Some(crate::proxy::openai::ToolChoice::Mode("auto".to_string()));

        let err = gw.chat_completion(req).await.unwrap_err();
        assert!(
            err.to_string().contains("cannot broker tool calls"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn exec_gateway_rejects_tool_bearing_requests() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(f, "#!/bin/sh\ncat\n").unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["-".to_string()],
            HashMap::new(),
        );

        let mut req = request("kimi-cli", "use a tool");
        req.tools = Some(vec![crate::proxy::openai::ToolDefinition {
            r#type: "function".to_string(),
            function: crate::proxy::openai::FunctionDefinition {
                name: "lookup".to_string(),
                description: Some("look up a value".to_string()),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        }]);

        let err = gw.chat_completion(req).await.unwrap_err();
        assert!(
            err.to_string().contains("cannot broker tool calls"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn exec_gateway_rejects_tool_call_history() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(f, "#!/bin/sh\ncat\n").unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["-".to_string()],
            HashMap::new(),
        );

        let mut req = request("kimi-cli", "use a tool");
        req.messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("tool result".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            reasoning: None,
            reasoning_content: None,
        });

        let err = gw.chat_completion(req).await.unwrap_err();
        assert!(
            err.to_string().contains("tool-call history"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn exec_gateway_rejects_legacy_function_call_history() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&script)
            .unwrap();
        writeln!(f, "#!/bin/sh\ncat\n").unwrap();
        drop(f);

        let gw = ExecGateway::new(
            GatewayConfig::default(),
            script.to_string_lossy().to_string(),
            vec!["-".to_string()],
            HashMap::new(),
        );

        let mut req = request("kimi-cli", "use a legacy function");
        req.messages.push(ChatMessage {
            role: "function".to_string(),
            content: Some(MessageContent::Text("legacy function result".to_string())),
            name: Some("lookup".to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_content: None,
        });

        let err = gw.chat_completion(req).await.unwrap_err();
        assert!(
            err.to_string().contains("tool-call history"),
            "unexpected error: {err}"
        );
    }
}
