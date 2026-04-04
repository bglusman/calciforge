//! Claude Code ACP (Agent Communication Protocol) integration for nonzeroclaw.
//!
//! This channel spawns a `claude` CLI subprocess and communicates with it via
//! stdin/stdout using a simple line-based protocol. It enables using Claude Code
//! as a coding agent within the NZC channel infrastructure.
//!
//! ## Protocol
//! The ACP protocol used with `--print` mode is line-based:
//! - Claude emits its response directly on stdout when invoked with `--print`
//! - Tool calls appear in the output as structured JSON or XML blocks
//! - The process exits (or can be re-spawned) after each prompt
//!
//! ## Configuration
//! ```toml
//! [channels_config.claude_acp]
//! enabled = true
//! claude_path = "/usr/local/bin/claude"
//! workspace_dir = "/home/user/workspace"
//! permission_mode = "bypassPermissions"
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const DEFAULT_CLAUDE_PATH: &str = "claude";
const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";
/// Timeout for a single Claude Code invocation (10 minutes).
const DEFAULT_INVOCATION_TIMEOUT_SECS: u64 = 600;
/// Poll interval when listening in the wait loop.
const LISTEN_POLL_INTERVAL: Duration = Duration::from_secs(5);

// ─── ACP Protocol Parsing ────────────────────────────────────────────────────

/// An event emitted by Claude Code during an ACP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpEvent {
    /// A client (user/assistant) message block.
    Client(String),
    /// A tool invocation block.
    Tool(String),
    /// A thinking/reasoning block.
    Thinking(String),
    /// Session completion marker.
    Done,
    /// Any other output line.
    Raw(String),
}

/// Parse a stream of ACP-formatted lines into [`AcpEvent`]s.
///
/// Recognises the following block markers:
/// - `[client]` … `[end]` → [`AcpEvent::Client`]
/// - `[tool]` … `[end]` → [`AcpEvent::Tool`]
/// - `[thinking]` … `[end]` → [`AcpEvent::Thinking`]
/// - `[done]` → [`AcpEvent::Done`]
///
/// Lines not matching any marker are emitted as [`AcpEvent::Raw`].
pub fn parse_acp_events(input: &str) -> Vec<AcpEvent> {
    let mut events = Vec::new();
    let mut current_block: Option<(&str, Vec<String>)> = None;

    for line in input.lines() {
        match line.trim() {
            "[client]" => {
                current_block = Some(("client", Vec::new()));
            }
            "[tool]" => {
                current_block = Some(("tool", Vec::new()));
            }
            "[thinking]" => {
                current_block = Some(("thinking", Vec::new()));
            }
            "[done]" => {
                if let Some((tag, lines)) = current_block.take() {
                    events.push(block_to_event(tag, lines));
                }
                events.push(AcpEvent::Done);
            }
            "[end]" => {
                if let Some((tag, lines)) = current_block.take() {
                    events.push(block_to_event(tag, lines));
                }
            }
            other => {
                if let Some((_, ref mut lines)) = current_block {
                    lines.push(other.to_string());
                } else {
                    events.push(AcpEvent::Raw(other.to_string()));
                }
            }
        }
    }

    // Flush any unclosed block
    if let Some((tag, lines)) = current_block.take() {
        events.push(block_to_event(tag, lines));
    }

    events
}

fn block_to_event(tag: &str, lines: Vec<String>) -> AcpEvent {
    let content = lines.join("\n");
    match tag {
        "client" => AcpEvent::Client(content),
        "tool" => AcpEvent::Tool(content),
        "thinking" => AcpEvent::Thinking(content),
        _ => AcpEvent::Raw(content),
    }
}

/// Extract the final text response from a list of ACP events.
///
/// Prefers the last [`AcpEvent::Client`] block, falling back to
/// concatenating all [`AcpEvent::Raw`] lines if no client block exists.
pub fn extract_acp_response(events: &[AcpEvent]) -> String {
    // Find the last client block (Claude's final response to the user)
    for event in events.iter().rev() {
        if let AcpEvent::Client(text) = event {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
    }

    // Fall back to raw output
    let raw: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AcpEvent::Raw(s) = e {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();

    raw.join("\n").trim().to_string()
}

// ─── Channel ─────────────────────────────────────────────────────────────────

/// Claude Code ACP channel.
///
/// Spawns `claude --print --permission-mode bypassPermissions` as a subprocess,
/// writes the prompt to stdin, reads the full response from stdout, and
/// returns control to the caller.
///
/// Each `send` call spawns a new subprocess. This is intentional — Claude Code
/// is a stateless CLI tool and the channel does not maintain a persistent
/// subprocess between calls.
pub struct ClaudeAcpChannel {
    /// Path to the `claude` executable.
    pub claude_path: PathBuf,
    /// Working directory for the spawned subprocess.
    pub workspace_dir: PathBuf,
    /// Permission mode passed as `--permission-mode <mode>`.
    pub permission_mode: String,
    /// Maximum time to wait for a subprocess to complete.
    pub timeout_secs: u64,
    /// Additional CLI arguments to pass to the subprocess.
    pub extra_args: Vec<String>,
}

impl ClaudeAcpChannel {
    /// Create a new ACP channel with default settings.
    pub fn new(claude_path: impl AsRef<Path>, workspace_dir: impl AsRef<Path>) -> Self {
        Self {
            claude_path: claude_path.as_ref().to_path_buf(),
            workspace_dir: workspace_dir.as_ref().to_path_buf(),
            permission_mode: DEFAULT_PERMISSION_MODE.to_string(),
            timeout_secs: DEFAULT_INVOCATION_TIMEOUT_SECS,
            extra_args: Vec::new(),
        }
    }

    /// Create a channel from a [`ClaudeAcpConfig`].
    pub fn from_config(config: &crate::config::schema::ClaudeAcpConfig) -> Self {
        Self {
            claude_path: PathBuf::from(if config.claude_path.is_empty() {
                DEFAULT_CLAUDE_PATH.to_string()
            } else {
                config.claude_path.clone()
            }),
            workspace_dir: PathBuf::from(&config.workspace_dir),
            permission_mode: if config.permission_mode.is_empty() {
                DEFAULT_PERMISSION_MODE.to_string()
            } else {
                config.permission_mode.clone()
            },
            timeout_secs: if config.timeout_secs == 0 {
                DEFAULT_INVOCATION_TIMEOUT_SECS
            } else {
                config.timeout_secs
            },
            extra_args: config.extra_args.clone(),
        }
    }

    /// Set the permission mode.
    #[must_use]
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = mode.into();
        self
    }

    /// Set the timeout for subprocess execution.
    #[must_use]
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Add extra CLI arguments.
    #[must_use]
    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Spawn a Claude Code subprocess for a single prompt.
    ///
    /// Returns the full stdout output after the process exits.
    /// The process is killed if it doesn't finish within `timeout_secs`.
    pub async fn run_prompt(&self, prompt: &str) -> Result<String> {
        let mut cmd = Command::new(&self.claude_path);
        cmd.arg("--print")
            .arg("--permission-mode")
            .arg(&self.permission_mode);

        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        cmd.current_dir(&self.workspace_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child: Child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn claude: {}", self.claude_path.display()))?;

        // Write the prompt to stdin then close it
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Failed to write prompt to claude stdin")?;
            // Drop closes stdin, signalling EOF to the subprocess
        }

        // Collect stdout
        let stdout_output = if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut output = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => output.push_str(&line),
                    Err(e) => {
                        tracing::warn!("Error reading claude stdout: {e}");
                        break;
                    }
                }
            }
            output
        } else {
            String::new()
        };

        // Wait for the process with a timeout
        let timeout = Duration::from_secs(self.timeout_secs);
        let exit_status =
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => status,
                Ok(Err(e)) => {
                    bail!("Claude process wait failed: {e}");
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = self.timeout_secs,
                        "Claude process timed out; killing"
                    );
                    bail!(
                        "Claude process timed out after {}s",
                        self.timeout_secs
                    );
                }
            };

        if !exit_status.success() {
            tracing::warn!(
                exit_code = ?exit_status.code(),
                "Claude process exited with non-zero status"
            );
        }

        Ok(stdout_output)
    }

    /// Check whether the `claude` executable is accessible.
    async fn claude_executable_available(&self) -> bool {
        // Try `claude --version` as a lightweight check.
        let result = Command::new(&self.claude_path)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();

        match result {
            Ok(mut child) => {
                let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                matches!(status, Ok(Ok(_)))
            }
            Err(_) => false,
        }
    }
}

// ─── Channel trait implementation ────────────────────────────────────────────

#[async_trait]
impl Channel for ClaudeAcpChannel {
    fn name(&self) -> &str {
        "claude_acp"
    }

    /// Send a message to Claude Code and discard the response.
    ///
    /// In most workflows the caller drives Claude Code through the
    /// `run_prompt` method directly; `send` is provided for compatibility
    /// with the NZC channel pipeline.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let output = self.run_prompt(&message.content).await?;
        tracing::debug!(
            preview = %&output[..output.len().min(120)],
            "Claude ACP send completed"
        );
        Ok(())
    }

    /// Listen for incoming messages.
    ///
    /// Claude Code is a CLI tool — it doesn't push messages. This implementation
    /// keeps the loop alive. Use `run_prompt` directly in coding workflows.
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            claude_path = %self.claude_path.display(),
            workspace = %self.workspace_dir.display(),
            "Claude ACP channel listening (CLI mode — no push messages)"
        );

        loop {
            tokio::time::sleep(LISTEN_POLL_INTERVAL).await;

            if tx.is_closed() {
                tracing::info!("Claude ACP channel: message bus closed, exiting listen loop");
                return Ok(());
            }
        }
    }

    /// Verify the `claude` executable is available.
    async fn health_check(&self) -> bool {
        self.claude_executable_available().await
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ACP event parsing ──────────────────────────────────────────

    #[test]
    fn parse_acp_events_empty_input() {
        let events = parse_acp_events("");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_acp_events_done_only() {
        let events = parse_acp_events("[done]");
        assert_eq!(events, vec![AcpEvent::Done]);
    }

    #[test]
    fn parse_acp_events_client_block() {
        let input = "[client]\nHello from Claude\n[end]";
        let events = parse_acp_events(input);
        assert_eq!(events, vec![AcpEvent::Client("Hello from Claude".into())]);
    }

    #[test]
    fn parse_acp_events_tool_block() {
        let input = "[tool]\nls -la\n[end]";
        let events = parse_acp_events(input);
        assert_eq!(events, vec![AcpEvent::Tool("ls -la".into())]);
    }

    #[test]
    fn parse_acp_events_thinking_block() {
        let input = "[thinking]\nI should check the filesystem.\n[end]";
        let events = parse_acp_events(input);
        assert_eq!(
            events,
            vec![AcpEvent::Thinking("I should check the filesystem.".into())]
        );
    }

    #[test]
    fn parse_acp_events_full_sequence() {
        let input = "[client]\nHello\n[end]\n[tool]\nls\n[end]\n[done]";
        let events = parse_acp_events(input);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], AcpEvent::Client("Hello".into()));
        assert_eq!(events[1], AcpEvent::Tool("ls".into()));
        assert_eq!(events[2], AcpEvent::Done);
    }

    #[test]
    fn parse_acp_events_raw_lines() {
        let input = "some raw output\nmore raw output";
        let events = parse_acp_events(input);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], AcpEvent::Raw("some raw output".into()));
        assert_eq!(events[1], AcpEvent::Raw("more raw output".into()));
    }

    #[test]
    fn parse_acp_events_multiline_client_block() {
        let input = "[client]\nLine 1\nLine 2\nLine 3\n[end]";
        let events = parse_acp_events(input);
        assert_eq!(
            events,
            vec![AcpEvent::Client("Line 1\nLine 2\nLine 3".into())]
        );
    }

    #[test]
    fn parse_acp_events_unclosed_block_flushed() {
        let input = "[client]\nUnclosed content";
        let events = parse_acp_events(input);
        assert_eq!(events, vec![AcpEvent::Client("Unclosed content".into())]);
    }

    #[test]
    fn parse_acp_events_done_closes_open_block() {
        let input = "[client]\nContent\n[done]";
        let events = parse_acp_events(input);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], AcpEvent::Client("Content".into()));
        assert_eq!(events[1], AcpEvent::Done);
    }

    // ── Response extraction ────────────────────────────────────────

    #[test]
    fn extract_acp_response_prefers_last_client_block() {
        let events = vec![
            AcpEvent::Client("First response".into()),
            AcpEvent::Client("Final response".into()),
        ];
        assert_eq!(extract_acp_response(&events), "Final response");
    }

    #[test]
    fn extract_acp_response_falls_back_to_raw() {
        let events = vec![
            AcpEvent::Raw("raw output 1".into()),
            AcpEvent::Raw("raw output 2".into()),
        ];
        assert_eq!(extract_acp_response(&events), "raw output 1\nraw output 2");
    }

    #[test]
    fn extract_acp_response_skips_empty_client_blocks() {
        let events = vec![
            AcpEvent::Client("   ".into()), // whitespace-only
            AcpEvent::Raw("fallback raw".into()),
        ];
        assert_eq!(extract_acp_response(&events), "fallback raw");
    }

    #[test]
    fn extract_acp_response_empty_events() {
        let events: Vec<AcpEvent> = vec![];
        assert_eq!(extract_acp_response(&events), "");
    }

    #[test]
    fn extract_acp_response_ignores_tool_and_thinking_blocks() {
        let events = vec![
            AcpEvent::Tool("ls -la".into()),
            AcpEvent::Thinking("I need to look at the directory".into()),
            AcpEvent::Client("Here is the listing".into()),
        ];
        assert_eq!(extract_acp_response(&events), "Here is the listing");
    }

    // ── Channel construction ───────────────────────────────────────

    #[test]
    fn channel_name_is_claude_acp() {
        let ch = ClaudeAcpChannel::new("/usr/local/bin/claude", "/tmp");
        assert_eq!(ch.name(), "claude_acp");
    }

    #[test]
    fn from_config_uses_defaults_for_empty_fields() {
        let config = crate::config::schema::ClaudeAcpConfig {
            enabled: true,
            claude_path: String::new(),  // empty → default
            workspace_dir: "/tmp".to_string(),
            permission_mode: String::new(), // empty → default
            timeout_secs: 0,             // zero → default
            extra_args: vec![],
        };
        let ch = ClaudeAcpChannel::from_config(&config);
        assert_eq!(ch.claude_path, PathBuf::from(DEFAULT_CLAUDE_PATH));
        assert_eq!(ch.permission_mode, DEFAULT_PERMISSION_MODE);
        assert_eq!(ch.timeout_secs, DEFAULT_INVOCATION_TIMEOUT_SECS);
    }

    #[test]
    fn from_config_preserves_explicit_values() {
        let config = crate::config::schema::ClaudeAcpConfig {
            enabled: true,
            claude_path: "/custom/path/claude".to_string(),
            workspace_dir: "/my/workspace".to_string(),
            permission_mode: "default".to_string(),
            timeout_secs: 120,
            extra_args: vec!["--debug".to_string()],
        };
        let ch = ClaudeAcpChannel::from_config(&config);
        assert_eq!(ch.claude_path, PathBuf::from("/custom/path/claude"));
        assert_eq!(ch.workspace_dir, PathBuf::from("/my/workspace"));
        assert_eq!(ch.permission_mode, "default");
        assert_eq!(ch.timeout_secs, 120);
        assert_eq!(ch.extra_args, vec!["--debug".to_string()]);
    }

    #[test]
    fn with_permission_mode_builder() {
        let ch = ClaudeAcpChannel::new("claude", "/tmp").with_permission_mode("default");
        assert_eq!(ch.permission_mode, "default");
    }

    #[test]
    fn with_timeout_builder() {
        let ch = ClaudeAcpChannel::new("claude", "/tmp").with_timeout(30);
        assert_eq!(ch.timeout_secs, 30);
    }

    #[test]
    fn with_extra_args_builder() {
        let ch = ClaudeAcpChannel::new("claude", "/tmp")
            .with_extra_args(vec!["--debug".to_string(), "--output-format".to_string()]);
        assert_eq!(ch.extra_args.len(), 2);
    }

    // ── Protocol roundtrip ─────────────────────────────────────────

    #[test]
    fn full_acp_protocol_roundtrip() {
        let input = "[client]\nHello\n[end]\n[tool]\nls\n[end]\n[done]";
        let events = parse_acp_events(input);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], AcpEvent::Client("Hello".into()));
        assert_eq!(events[1], AcpEvent::Tool("ls".into()));
        assert_eq!(events[2], AcpEvent::Done);
        let response = extract_acp_response(&events);
        assert_eq!(response, "Hello");
    }
}
