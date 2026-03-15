//! CliAdapter — dispatches to a CLI binary by spawning a subprocess.
//!
//! Used for IronClaw and any other agent that doesn't expose an HTTP server.
//!
//! # Protocol
//!
//! The message text is substituted into `{message}` placeholders in the `args`
//! template, and the binary is spawned. Stdout is captured as the response.
//! Stderr is forwarded to the tracing log at WARN level.
//!
//! # Argument length limiting
//!
//! Some CLI agents (e.g. IronClaw) have OS or internal limits on argument
//! length. When a context-augmented message would exceed `max_arg_chars` (default
//! 300), the adapter strips the `[Recent context:\n...\n]\n\n` preamble and
//! passes only the bare user message. This prevents silent failures caused by
//! overlong `-m` arguments.
//!
//! # Example config
//!
//! ```toml
//! [[agents]]
//! id = "ironclaw"
//! kind = "cli"
//! command = "/usr/local/bin/ironclaw"
//! args = ["run", "-m", "{message}"]
//! timeout_ms = 30000
//! env = { LLM_BACKEND = "openai_compatible", LLM_BASE_URL = "...", LLM_MODEL = "kimi-k2.5" }
//! ```

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter};

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Default maximum message length (in chars) allowed as a CLI arg.
/// When the augmented message exceeds this, the context preamble is stripped.
const DEFAULT_MAX_ARG_CHARS: usize = 300;
/// Placeholder string in args that gets replaced with the message.
const MESSAGE_PLACEHOLDER: &str = "{message}";

/// CLI adapter — spawns a binary and reads its stdout as the response.
pub struct CliAdapter {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Duration,
    /// Maximum character length for the message substituted into args.
    /// When the message (after context augmentation) exceeds this limit, the
    /// context preamble is stripped and only the bare user message is passed.
    max_arg_chars: usize,
}

impl CliAdapter {
    /// Create a new CLI adapter.
    ///
    /// - `command` — path to the binary (e.g. `/usr/local/bin/ironclaw`)
    /// - `args` — argument template; `{message}` placeholders are replaced at dispatch time
    /// - `env` — additional environment variables to set
    /// - `timeout_ms` — per-invocation timeout (`None` → 30 000 ms)
    /// - `max_arg_chars` — max chars allowed in a single arg (`None` → 300);
    ///   messages longer than this have their context preamble stripped
    pub fn new(
        command: String,
        args: Option<Vec<String>>,
        env: HashMap<String, String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self::with_max_arg_chars(command, args, env, timeout_ms, None)
    }

    /// Like [`new`] but with an explicit `max_arg_chars` override.
    pub fn with_max_arg_chars(
        command: String,
        args: Option<Vec<String>>,
        env: HashMap<String, String>,
        timeout_ms: Option<u64>,
        max_arg_chars: Option<usize>,
    ) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        // Default args: ["-m", "{message}"] when not specified
        let args = args.unwrap_or_else(|| {
            vec!["-m".to_string(), MESSAGE_PLACEHOLDER.to_string()]
        });
        let max_arg_chars = max_arg_chars.unwrap_or(DEFAULT_MAX_ARG_CHARS);
        Self { command, args, env, timeout, max_arg_chars }
    }

    /// Strip the `[Recent context:\n...\n]\n\n` preamble from a message,
    /// returning only the bare user text that follows it.
    ///
    /// The preamble format is:
    /// ```text
    /// [Recent context:
    /// ...
    /// ]
    ///
    /// <actual message>
    /// ```
    ///
    /// Returns the original string unchanged if the preamble pattern is not found.
    fn strip_context_preamble(msg: &str) -> &str {
        // Find the last occurrence of "]\n\n" — the preamble closes with ']'
        // followed by a blank line before the actual message.
        if let Some(pos) = msg.rfind("]\n\n") {
            let after = &msg[pos + 3..]; // skip "]\n\n"
            if !after.is_empty() {
                return after;
            }
        }
        msg
    }

    /// Prepare the message for arg substitution.
    ///
    /// If `msg` is longer than `max_arg_chars`, strip the context preamble.
    fn effective_message<'a>(&self, msg: &'a str) -> &'a str {
        if msg.len() > self.max_arg_chars {
            let stripped = Self::strip_context_preamble(msg);
            if stripped.len() != msg.len() {
                debug!(
                    original_len = msg.len(),
                    stripped_len = stripped.len(),
                    max_arg_chars = self.max_arg_chars,
                    "cli: stripped context preamble (message too long for arg)"
                );
            }
            stripped
        } else {
            msg
        }
    }

    /// Substitute `{message}` in each arg string.
    fn build_args(&self, msg: &str) -> Vec<String> {
        let effective = self.effective_message(msg);
        self.args
            .iter()
            .map(|a| a.replace(MESSAGE_PLACEHOLDER, effective))
            .collect()
    }
}

#[async_trait]
impl AgentAdapter for CliAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let args = self.build_args(msg);

        info!(
            command = %self.command,
            args = ?args,
            "cli dispatch"
        );
        debug!(msg = %msg, "outbound message");

        let mut cmd = Command::new(&self.command);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set env vars from config
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            AdapterError::Unavailable(format!("failed to spawn {}: {}", self.command, e))
        })?;

        // Capture stdout and stderr handles before waiting
        let mut stdout_handle = child.stdout.take().map(tokio::io::BufReader::new);
        let mut stderr_handle = child.stderr.take().map(tokio::io::BufReader::new);

        // Wait with timeout
        let wait_result = tokio::time::timeout(self.timeout, child.wait()).await;

        // Read stderr regardless of outcome (for logging)
        let stderr_text = if let Some(ref mut handle) = stderr_handle {
            let mut buf = String::new();
            let _ = handle.read_to_string(&mut buf).await;
            buf
        } else {
            String::new()
        };

        if !stderr_text.trim().is_empty() {
            warn!(
                command = %self.command,
                stderr = %stderr_text.trim(),
                "cli agent stderr"
            );
        }

        let exit_status = match wait_result {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => {
                return Err(AdapterError::Unavailable(format!(
                    "cli process error: {}",
                    e
                )));
            }
            Err(_elapsed) => {
                return Err(AdapterError::Timeout);
            }
        };

        if !exit_status.success() {
            let code = exit_status.code().unwrap_or(-1);
            return Err(AdapterError::Protocol(format!(
                "cli process exited with code {}",
                code
            )));
        }

        // Read stdout
        let stdout_text = if let Some(ref mut handle) = stdout_handle {
            let mut buf = String::new();
            let _ = handle.read_to_string(&mut buf).await;
            buf
        } else {
            String::new()
        };

        let response = stdout_text.trim().to_string();
        if response.is_empty() {
            return Err(AdapterError::Protocol(
                "cli process produced no output".to_string(),
            ));
        }

        info!(command = %self.command, response_len = %response.len(), "cli: received response");
        debug!(response = %response, "cli response");

        Ok(response)
    }

    fn kind(&self) -> &'static str {
        "cli"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter(cmd: &str, args: Option<Vec<&str>>) -> CliAdapter {
        CliAdapter::new(
            cmd.to_string(),
            args.map(|a| a.iter().map(|s| s.to_string()).collect()),
            HashMap::new(),
            Some(5000),
        )
    }

    #[test]
    fn test_kind_is_cli() {
        let adapter = make_adapter("/bin/echo", None);
        assert_eq!(adapter.kind(), "cli");
    }

    #[test]
    fn test_build_args_substitutes_message() {
        let adapter = make_adapter(
            "/usr/local/bin/ironclaw",
            Some(vec!["run", "-m", "{message}"]),
        );
        let args = adapter.build_args("hello world");
        assert_eq!(args, vec!["run", "-m", "hello world"]);
    }

    #[test]
    fn test_build_args_multiple_placeholders() {
        let adapter = CliAdapter::new(
            "/bin/echo".to_string(),
            Some(vec![
                "{message}".to_string(),
                "--input".to_string(),
                "{message}".to_string(),
            ]),
            HashMap::new(),
            Some(5000),
        );
        let args = adapter.build_args("ping");
        assert_eq!(args, vec!["ping", "--input", "ping"]);
    }

    #[test]
    fn test_build_args_no_placeholder_passes_through() {
        let adapter = make_adapter("/bin/echo", Some(vec!["--version"]));
        let args = adapter.build_args("ignored message");
        // When no placeholder, args are unchanged
        assert_eq!(args, vec!["--version"]);
    }

    #[test]
    fn test_default_args_when_none() {
        let adapter = CliAdapter::new(
            "/bin/echo".to_string(),
            None,
            HashMap::new(),
            None,
        );
        // Default: ["-m", "{message}"]
        let args = adapter.build_args("test");
        assert_eq!(args, vec!["-m", "test"]);
    }

    #[test]
    fn test_default_timeout() {
        let adapter = CliAdapter::new(
            "/bin/echo".to_string(),
            None,
            HashMap::new(),
            None,
        );
        assert_eq!(adapter.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn test_default_timeout_is_30s() {
        assert_eq!(DEFAULT_TIMEOUT_MS, 30_000, "IronClaw default timeout should be 30s");
    }

    #[test]
    fn test_env_vars_set() {
        let mut env = HashMap::new();
        env.insert("LLM_BACKEND".to_string(), "openai_compatible".to_string());
        env.insert("LLM_MODEL".to_string(), "kimi-k2.5".to_string());
        let adapter = CliAdapter::new(
            "/bin/echo".to_string(),
            None,
            env.clone(),
            None,
        );
        assert_eq!(adapter.env["LLM_BACKEND"], "openai_compatible");
        assert_eq!(adapter.env["LLM_MODEL"], "kimi-k2.5");
    }

    // --- preamble stripping tests ---

    #[test]
    fn test_strip_preamble_with_context() {
        let msg = "[Recent context:\nBrian: hi\nlibrarian: hello\n]\n\nactual user message";
        let stripped = CliAdapter::strip_context_preamble(msg);
        assert_eq!(stripped, "actual user message");
    }

    #[test]
    fn test_strip_preamble_no_preamble_unchanged() {
        let msg = "just a plain message with no context";
        let stripped = CliAdapter::strip_context_preamble(msg);
        assert_eq!(stripped, msg);
    }

    #[test]
    fn test_strip_preamble_multiline_context() {
        let preamble = "[Recent context:\nBrian: msg1\nagent: resp1\nBrian: msg2\nagent: resp2\n]\n\nthe real question";
        let stripped = CliAdapter::strip_context_preamble(preamble);
        assert_eq!(stripped, "the real question");
    }

    #[test]
    fn test_effective_message_under_limit_unchanged() {
        let adapter = CliAdapter::with_max_arg_chars(
            "/bin/echo".to_string(), None, HashMap::new(), None, Some(1000),
        );
        let short_msg = "short message";
        assert_eq!(adapter.effective_message(short_msg), short_msg);
    }

    #[test]
    fn test_effective_message_over_limit_strips_preamble() {
        let adapter = CliAdapter::with_max_arg_chars(
            "/bin/echo".to_string(), None, HashMap::new(), None, Some(10),
        );
        let augmented = "[Recent context:\nBrian: something long\nlibrarian: a reply\n]\n\nshort msg";
        let effective = adapter.effective_message(augmented);
        assert_eq!(effective, "short msg");
    }

    #[test]
    fn test_effective_message_over_limit_no_preamble_unchanged() {
        // Message is long but has no context preamble — return as-is
        let adapter = CliAdapter::with_max_arg_chars(
            "/bin/echo".to_string(), None, HashMap::new(), None, Some(10),
        );
        let long_plain = "this is a long plain message without any preamble present";
        assert_eq!(adapter.effective_message(long_plain), long_plain);
    }

    #[test]
    fn test_build_args_strips_preamble_when_over_limit() {
        let adapter = CliAdapter::with_max_arg_chars(
            "/usr/local/bin/ironclaw".to_string(),
            Some(vec!["run".to_string(), "-m".to_string(), "{message}".to_string()]),
            HashMap::new(),
            None,
            Some(50), // small limit to force stripping
        );
        let augmented = "[Recent context:\nBrian: hi\nlibrarian: hello\n]\n\nwhat is 2+2?";
        let args = adapter.build_args(augmented);
        assert_eq!(args, vec!["run", "-m", "what is 2+2?"]);
    }

    #[test]
    fn test_default_max_arg_chars() {
        let adapter = CliAdapter::new("/bin/echo".to_string(), None, HashMap::new(), None);
        assert_eq!(adapter.max_arg_chars, DEFAULT_MAX_ARG_CHARS);
    }

    #[tokio::test]
    async fn test_dispatch_echo_returns_output() {
        // Use /bin/echo with the message as arg
        let adapter = CliAdapter::new(
            "/bin/echo".to_string(),
            Some(vec!["{message}".to_string()]),
            HashMap::new(),
            Some(5000),
        );
        let result = adapter.dispatch("pong").await;
        assert!(result.is_ok(), "echo should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "pong");
    }

    #[tokio::test]
    async fn test_dispatch_nonexistent_binary_returns_unavailable() {
        let adapter = CliAdapter::new(
            "/usr/local/bin/does-not-exist-xyzzy".to_string(),
            None,
            HashMap::new(),
            Some(2000),
        );
        let result = adapter.dispatch("hello").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(_) => {}
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_exit_nonzero_returns_protocol_error() {
        // /bin/false always exits 1
        let adapter = CliAdapter::new(
            "/bin/false".to_string(),
            Some(vec![]),
            HashMap::new(),
            Some(2000),
        );
        let result = adapter.dispatch("hello").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Protocol(msg) => {
                assert!(msg.contains("exited with code"), "got: {}", msg);
            }
            other => panic!("expected Protocol, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_timeout() {
        // sleep 10 seconds but timeout after 100ms
        let adapter = CliAdapter::new(
            "/bin/sleep".to_string(),
            Some(vec!["10".to_string()]),
            HashMap::new(),
            Some(100), // 100ms timeout
        );
        let result = adapter.dispatch("irrelevant").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Timeout => {}
            other => panic!("expected Timeout, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_strips_preamble_before_passing_to_binary() {
        // Echo outputs exactly what's passed — verify preamble is stripped
        let adapter = CliAdapter::with_max_arg_chars(
            "/bin/echo".to_string(),
            Some(vec!["{message}".to_string()]),
            HashMap::new(),
            Some(5000),
            Some(50), // force stripping
        );
        let augmented = "[Recent context:\nBrian: hi\nlibrarian: hey\n]\n\necho this";
        let result = adapter.dispatch(augmented).await;
        assert!(result.is_ok(), "should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "echo this");
    }
}
