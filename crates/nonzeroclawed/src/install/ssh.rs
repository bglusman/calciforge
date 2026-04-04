//! SSH operations for the NonZeroClawed installer.
//!
//! All SSH interactions go through the [`SshClient`] trait, which has two
//! implementations:
//! - [`RealSshClient`] — shells out to `ssh(1)` with `-i <key>` and
//!   `StrictHostKeyChecking=accept-new` (suitable for first-time installs).
//! - [`MockSshClient`] — in-memory stub for unit tests.
//!
//! # Injection safety
//!
//! Remote commands are built from structured arguments and then **shell-quoted**
//! via [`shell_quote`] before being assembled into the SSH command string.
//! User-supplied strings (host, path, file contents) must always go through
//! `shell_quote` before being interpolated into a shell command.
//!
//! The property tests in `model.rs` verify that `shell_quote` never lets bare
//! single-quotes through.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstract over SSH operations so tests can inject a mock.
pub trait SshClient: Send + Sync {
    /// Run a shell command on the remote host and return its stdout.
    ///
    /// `host` is `user@hostname` or `hostname`.
    /// `key` is the path to the SSH private key (may be `None` for default-key use).
    /// `command` is a **pre-quoted** shell command (use [`shell_quote`] on all
    /// user-supplied components before calling).
    fn run(&self, host: &str, key: Option<&Path>, command: &str) -> Result<SshOutput>;

    /// Read the contents of a remote file.
    fn read_file(&self, host: &str, key: Option<&Path>, remote_path: &str) -> Result<String> {
        let cmd = format!("cat {}", shell_quote(remote_path));
        let out = self.run(host, key, &cmd)?;
        Ok(out.stdout)
    }

    /// Write content to a remote file (overwrites).
    ///
    /// Uses `printf '%s' <quoted_content> > <quoted_path>` to avoid
    /// newline/escape issues with `echo`.
    fn write_file(
        &self,
        host: &str,
        key: Option<&Path>,
        remote_path: &str,
        content: &str,
    ) -> Result<()> {
        // Use a here-document approach via base64 to avoid quoting nightmares
        // with arbitrary file contents.
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        let cmd = format!(
            "echo {} | base64 -d > {}",
            shell_quote(&b64),
            shell_quote(remote_path),
        );
        let out = self.run(host, key, &cmd)?;
        if !out.success {
            bail!("write_file failed on {}: {}", host, out.stderr.trim());
        }
        Ok(())
    }

    /// Copy a remote file to a backup path.
    ///
    /// Returns the backup path used.
    fn backup_file(
        &self,
        host: &str,
        key: Option<&Path>,
        remote_path: &str,
        backup_path: &str,
    ) -> Result<()> {
        let cmd = format!(
            "cp {} {}",
            shell_quote(remote_path),
            shell_quote(backup_path),
        );
        let out = self.run(host, key, &cmd)?;
        if !out.success {
            bail!("backup_file failed on {}: {}", host, out.stderr.trim());
        }
        Ok(())
    }

    /// Verify a remote file exists and is non-empty.
    fn verify_file_exists(
        &self,
        host: &str,
        key: Option<&Path>,
        remote_path: &str,
    ) -> Result<bool> {
        let cmd = format!(
            "test -s {} && echo EXISTS || echo MISSING",
            shell_quote(remote_path)
        );
        let out = self.run(host, key, &cmd)?;
        Ok(out.stdout.trim() == "EXISTS")
    }

    /// Restore a remote file from a backup.
    fn restore_backup(
        &self,
        host: &str,
        key: Option<&Path>,
        backup_path: &str,
        original_path: &str,
    ) -> Result<()> {
        let cmd = format!(
            "cp {} {}",
            shell_quote(backup_path),
            shell_quote(original_path),
        );
        let out = self.run(host, key, &cmd)?;
        if !out.success {
            bail!("restore_backup failed on {}: {}", host, out.stderr.trim());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// Output from an SSH command execution.
#[derive(Debug, Clone)]
pub struct SshOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Real implementation
// ---------------------------------------------------------------------------

/// SSH client that shells out to `ssh(1)`.
pub struct RealSshClient;

impl SshClient for RealSshClient {
    fn run(&self, host: &str, key: Option<&Path>, command: &str) -> Result<SshOutput> {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o")
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("BatchMode=yes");

        if let Some(key_path) = key {
            cmd.arg("-i").arg(key_path);
        }

        cmd.arg(host);
        cmd.arg(command);

        let output = cmd
            .output()
            .with_context(|| format!("failed to spawn ssh for host '{}'", host))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        Ok(SshOutput {
            stdout,
            stderr,
            exit_code,
            success,
        })
    }
}

// ---------------------------------------------------------------------------
// Mock implementation (for tests)
// ---------------------------------------------------------------------------

/// An SSH interaction recorded for verification in tests.
#[derive(Debug, Clone)]
pub struct SshCall {
    pub host: String,
    pub key: Option<PathBuf>,
    pub command: String,
}

/// Mock SSH client that returns canned responses and records calls.
///
/// Uses interior mutability (`std::sync::Mutex`) so it can be shared via
/// `Arc` across the trait interface (which requires `&self`).
pub struct MockSshClient {
    /// Queued responses, consumed in order. When exhausted, returns a default
    /// success with empty stdout.
    responses: std::sync::Mutex<Vec<SshOutput>>,
    /// All calls made, for assertion in tests.
    pub calls: std::sync::Mutex<Vec<SshCall>>,
}

impl MockSshClient {
    pub fn new() -> Self {
        Self {
            responses: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Push a response to be returned by the next `run` call.
    pub fn push_response(&self, output: SshOutput) {
        self.responses.lock().unwrap().push(output);
    }

    /// Push a successful response with given stdout.
    pub fn push_success(&self, stdout: &str) {
        self.push_response(SshOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: 0,
            success: true,
        });
    }

    /// Push a failure response with given stderr.
    pub fn push_failure(&self, stderr: &str) {
        self.push_response(SshOutput {
            stdout: String::new(),
            stderr: stderr.to_string(),
            exit_code: 1,
            success: false,
        });
    }

    /// Number of calls recorded.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// Get recorded calls.
    pub fn recorded_calls(&self) -> Vec<SshCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockSshClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SshClient for MockSshClient {
    fn run(&self, host: &str, key: Option<&Path>, command: &str) -> Result<SshOutput> {
        self.calls.lock().unwrap().push(SshCall {
            host: host.to_string(),
            key: key.map(PathBuf::from),
            command: command.to_string(),
        });

        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            // Default: success with empty stdout.
            Ok(SshOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                success: true,
            })
        } else {
            Ok(responses.remove(0))
        }
    }
}

// ---------------------------------------------------------------------------
// shell_quote
// ---------------------------------------------------------------------------

/// Wrap a string in single-quotes for safe shell interpolation.
///
/// Single-quotes in the input are escaped via the `'\''` idiom:
/// close the quote, insert an escaped single-quote, reopen the quote.
///
/// This is the POSIX-portable approach and works on any `sh`-compatible shell.
///
/// # Examples
///
/// ```
/// use nonzeroclawed::install::ssh::shell_quote;
/// assert_eq!(shell_quote("hello world"), "'hello world'");
/// assert_eq!(shell_quote("it's a test"), "'it'\\''s a test'");
/// assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
/// ```
pub fn shell_quote(s: &str) -> String {
    // Replace every `'` with `'\''`
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

// ---------------------------------------------------------------------------
// SSH connectivity test
// ---------------------------------------------------------------------------

/// Test SSH connectivity to a host by running `echo OK`.
///
/// Returns `Ok(())` if the connection succeeds and stdout contains "OK".
/// Returns `Err` with a descriptive message on failure.
pub fn test_connectivity(client: &dyn SshClient, host: &str, key: Option<&Path>) -> Result<()> {
    let out = client
        .run(host, key, "echo OK")
        .with_context(|| format!("SSH connectivity test to '{}' failed", host))?;

    if !out.success {
        bail!(
            "SSH connectivity test to '{}' failed (exit {}): {}",
            host,
            out.exit_code,
            out.stderr.trim()
        );
    }

    // Check for exact "OK" token (not just substring, to avoid "NOTOK" matching).
    let stdout_trimmed = out.stdout.trim();
    let has_ok = stdout_trimmed.split_whitespace().any(|token| token == "OK");
    if !has_ok {
        bail!(
            "SSH connectivity test to '{}' succeeded but unexpected stdout: {:?}",
            host,
            stdout_trimmed
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Version detection
// ---------------------------------------------------------------------------

/// Detect the version of a remote OpenClaw installation.
///
/// Reads `meta.lastTouchedVersion` from `openclaw.json` via `ssh + jq`, or
/// falls back to reading the raw JSON with a grep heuristic.
pub fn detect_openclaw_version(
    client: &dyn SshClient,
    host: &str,
    key: Option<&Path>,
    config_path: &str,
) -> Result<Option<String>> {
    // Try jq first (most reliable).
    let jq_cmd = format!(
        "jq -r '.meta.lastTouchedVersion // .version // empty' {} 2>/dev/null || true",
        shell_quote(config_path)
    );
    let out = client.run(host, key, &jq_cmd)?;
    let version = out.stdout.trim().to_string();
    if !version.is_empty() && version != "null" {
        return Ok(Some(version));
    }

    // Fallback: grep for version-like strings.
    let grep_cmd = format!(
        "grep -o '\"[0-9]\\{{4\\}}\\.\\.[0-9]\\+\\.[0-9]\\+\"' {} 2>/dev/null | head -1 | tr -d '\"' || true",
        shell_quote(config_path)
    );
    let out = client.run(host, key, &grep_cmd)?;
    let version = out.stdout.trim().to_string();
    if !version.is_empty() {
        return Ok(Some(version));
    }

    Ok(None)
}

/// Detect the version of a remote NZC installation.
///
/// Runs `nzc --version` on the remote host.
pub fn detect_nzc_version(
    client: &dyn SshClient,
    host: &str,
    key: Option<&Path>,
) -> Result<Option<String>> {
    let out = client.run(
        host,
        key,
        "nzc --version 2>/dev/null || nonzeroclaw --version 2>/dev/null || true",
    )?;
    let version = out.stdout.trim().to_string();
    if version.is_empty() {
        return Ok(None);
    }
    // Parse "nzc 0.3.0" or "nonzeroclaw 0.3.0" → "0.3.0"
    let version = version.split_whitespace().last().unwrap_or("").to_string();
    if version.is_empty() {
        Ok(None)
    } else {
        Ok(Some(version))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_simple_string() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn shell_quote_with_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_quote_prevents_command_injection() {
        // A classic injection attempt
        let malicious = "$(rm -rf /)";
        let quoted = shell_quote(malicious);
        // Must be wrapped in single quotes — no interpolation possible
        assert!(quoted.starts_with('\''));
        assert!(quoted.ends_with('\''));
        // The $( must be inside quotes
        assert!(quoted.contains("$(rm -rf /)"));
    }

    #[test]
    fn shell_quote_prevents_backtick_injection() {
        let malicious = "`id`";
        let quoted = shell_quote(malicious);
        assert!(quoted.starts_with('\''));
        assert!(quoted.ends_with('\''));
    }

    #[test]
    fn shell_quote_prevents_semicolon_injection() {
        let malicious = "foo; rm -rf /";
        let quoted = shell_quote(malicious);
        assert!(quoted.starts_with('\''));
        assert!(quoted.ends_with('\''));
    }

    #[test]
    fn shell_quote_path_with_spaces() {
        let path = "/home/user/my docs/config.json";
        let quoted = shell_quote(path);
        assert_eq!(quoted, "'/home/user/my docs/config.json'");
    }

    #[test]
    fn mock_ssh_records_calls() {
        let client = MockSshClient::new();
        client.push_success("OK");
        let out = client.run("user@host", None, "echo OK").unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "OK");
        assert_eq!(client.call_count(), 1);
        let calls = client.recorded_calls();
        assert_eq!(calls[0].host, "user@host");
        assert_eq!(calls[0].command, "echo OK");
    }

    #[test]
    fn mock_ssh_default_success_when_no_responses() {
        let client = MockSshClient::new();
        let out = client.run("host", None, "true").unwrap();
        assert!(out.success);
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn mock_ssh_consume_responses_in_order() {
        let client = MockSshClient::new();
        client.push_success("first");
        client.push_success("second");
        let r1 = client.run("host", None, "cmd1").unwrap();
        let r2 = client.run("host", None, "cmd2").unwrap();
        assert_eq!(r1.stdout, "first");
        assert_eq!(r2.stdout, "second");
    }

    #[test]
    fn test_connectivity_success() {
        let client = MockSshClient::new();
        client.push_success("OK\n");
        test_connectivity(&client, "user@host", None).unwrap();
    }

    #[test]
    fn test_connectivity_failure_exit_code() {
        let client = MockSshClient::new();
        client.push_failure("Connection refused");
        let result = test_connectivity(&client, "user@host", None);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("Connection refused"), "got: {}", msg);
    }

    #[test]
    fn test_connectivity_wrong_stdout() {
        let client = MockSshClient::new();
        client.push_success("NOTOK");
        let result = test_connectivity(&client, "user@host", None);
        assert!(result.is_err());
    }

    #[test]
    fn read_file_via_mock() {
        let client = MockSshClient::new();
        client.push_success(r#"{"version": "2026.3.13"}"#);
        let content = client
            .read_file("host", None, "/etc/openclaw.json")
            .unwrap();
        assert!(content.contains("2026.3.13"));
        // Verify the command used cat and the quoted path
        let calls = client.recorded_calls();
        assert!(
            calls[0].command.contains("cat"),
            "should use cat: {}",
            calls[0].command
        );
        assert!(
            calls[0].command.contains("/etc/openclaw.json"),
            "should contain path: {}",
            calls[0].command
        );
    }

    #[test]
    fn backup_file_via_mock() {
        let client = MockSshClient::new();
        // Success response
        client.push_success("");
        client
            .backup_file(
                "host",
                None,
                "/etc/openclaw.json",
                "/etc/openclaw.json.bak.123",
            )
            .unwrap();
        let calls = client.recorded_calls();
        assert!(
            calls[0].command.contains("cp"),
            "should use cp: {}",
            calls[0].command
        );
    }

    #[test]
    fn backup_file_failure_returns_error() {
        let client = MockSshClient::new();
        client.push_failure("permission denied");
        let result = client.backup_file(
            "host",
            None,
            "/etc/openclaw.json",
            "/etc/openclaw.json.bak.123",
        );
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("permission denied"), "got: {}", msg);
    }

    #[test]
    fn verify_file_exists_present() {
        let client = MockSshClient::new();
        client.push_success("EXISTS\n");
        assert!(client
            .verify_file_exists("host", None, "/etc/file")
            .unwrap());
    }

    #[test]
    fn verify_file_exists_missing() {
        let client = MockSshClient::new();
        client.push_success("MISSING\n");
        assert!(!client
            .verify_file_exists("host", None, "/etc/file")
            .unwrap());
    }

    #[test]
    fn restore_backup_via_mock() {
        let client = MockSshClient::new();
        client.push_success("");
        client
            .restore_backup(
                "host",
                None,
                "/etc/openclaw.json.bak.123",
                "/etc/openclaw.json",
            )
            .unwrap();
        let calls = client.recorded_calls();
        assert!(calls[0].command.contains("cp"), "should use cp");
    }

    #[test]
    fn detect_openclaw_version_jq_path() {
        let client = MockSshClient::new();
        client.push_success("2026.3.13\n");
        let version =
            detect_openclaw_version(&client, "host", None, "/root/.openclaw/openclaw.json")
                .unwrap();
        assert_eq!(version, Some("2026.3.13".to_string()));
    }

    #[test]
    fn detect_openclaw_version_empty_returns_none() {
        let client = MockSshClient::new();
        client.push_success("\n"); // jq returns empty
        client.push_success("\n"); // grep fallback also empty
        let version =
            detect_openclaw_version(&client, "host", None, "/root/.openclaw/openclaw.json")
                .unwrap();
        assert!(version.is_none());
    }

    /// Injection safety: shell_quote on arbitrary ASCII printable strings
    /// must always produce a safely-quoted result.
    ///
    /// The POSIX single-quote escape idiom is `'\''`:
    ///   - close the current quote with `'`
    ///   - emit `\'` (escaped literal single-quote outside any quoting)
    ///   - reopen the quote with `'`
    ///
    /// This means the raw string `'<inner>'` may contain `'` characters as
    /// part of the escape sequence, which is correct and safe.  What matters
    /// is that the resulting string, when interpreted by `sh`, equals the
    /// original input — not that the raw bytes contain no single-quotes.
    ///
    /// We verify: (a) starts and ends with `'`, and (b) round-trips correctly
    /// for inputs without literal single-quotes (no escape needed → simple `'...'`).
    #[test]
    fn shell_quote_arbitrary_paths_no_injection() {
        // Inputs that do NOT contain single-quotes: must produce simple `'...'`
        let safe_inputs = &[
            "$(cat /etc/passwd)",
            "`id`",
            "${HOME}/.ssh/authorized_keys",
            "foo\nbar",
            "foo\tbar",
            "a && b",
            "a || b",
            "a | b",
            "> /etc/crontab",
        ];
        for input in safe_inputs {
            let quoted = shell_quote(input);
            assert!(quoted.starts_with('\''), "must start with ': {}", quoted);
            assert!(quoted.ends_with('\''), "must end with ': {}", quoted);
            // For inputs without single-quotes, inner must have no bare `'`
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !inner.contains('\''),
                "inner must not contain bare single-quote for input {:?}: {}",
                input,
                quoted
            );
        }

        // Inputs that DO contain single-quotes: must use `'\''` escape idiom.
        // Verify the output starts/ends with `'` and is NOT the bare input.
        let single_quote_inputs = &["'; rm -rf /; echo '", "it's a test", "'$(dangerous)'"];
        for input in single_quote_inputs {
            let quoted = shell_quote(input);
            assert!(quoted.starts_with('\''), "must start with ': {}", quoted);
            assert!(quoted.ends_with('\''), "must end with ': {}", quoted);
            // Must have used the escape idiom: contains `'\''`
            assert!(
                quoted.contains("'\\''"),
                "inputs with single-quotes must use escape idiom for {:?}: {}",
                input,
                quoted
            );
        }
    }

    // ── Property tests (hegel) ────────────────────────────────────────────────

    /// Property: `shell_quote` is semantically safe — it evaluates to the original
    /// string when interpreted by a real POSIX shell.
    ///
    /// This is the **semantic** injection-safety property.  The structural tests
    /// above verify that the output is well-formed (starts/ends with `'`, uses
    /// `'\''` escape idiom).  THIS test verifies the ultimate guarantee:
    ///
    ///   sh -c "printf '%s' <quoted>" == original_input
    ///
    /// A structural test could pass even if the quoting is wrong for edge cases.
    /// This test fails if a real shell would interpret the quoted string differently
    /// from what we put in — the only test that actually catches semantic bugs.
    ///
    /// Inputs are restricted to printable ASCII (no null bytes) because:
    /// 1. `printf '%s'` and argument passing can't survive null bytes in practice.
    /// 2. Null bytes in shell arguments are undefined behavior (POSIX).
    /// 3. The realistic threat model for shell injection is printable characters.
    ///
    /// NOTE: This test spawns `sh -c` subprocesses.  It is the only test in this
    /// codebase that does so — deliberately, because the semantic guarantee cannot
    /// be verified without an actual shell.
    #[hegel::test]
    fn prop_shell_quote_semantic_eval(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;
        use std::process::Command;

        // Generate arbitrary printable ASCII strings including metacharacters.
        // We restrict to printable ASCII (0x20–0x7e) to avoid null bytes and
        // control characters that shells handle unpredictably.
        let input = tc.draw(
            gs::text()
                .max_size(80)
                .filter(|s: &String| s.chars().all(|c| c.is_ascii() && c >= ' ' && c != '\x7f')),
        );

        let quoted = shell_quote(&input);

        // Build: printf '%s' <quoted>
        // We use printf '%s' rather than echo to avoid echo's escape processing.
        let shell_cmd = format!("printf '%s' {}", quoted);

        let output = Command::new("sh")
            .args(["-c", &shell_cmd])
            .output()
            .expect("sh -c must be available for shell_quote semantic test");

        // The shell must exit successfully.
        assert!(
            output.status.success(),
            "shell command failed for input {:?}: cmd={:?} stderr={:?}",
            input,
            shell_cmd,
            String::from_utf8_lossy(&output.stderr)
        );

        let shell_output = String::from_utf8_lossy(&output.stdout);

        // The shell output must equal the original input exactly.
        assert_eq!(
            shell_output,
            input.as_str(),
            "shell_quote semantic mismatch!\n\
             input:        {:?}\n\
             quoted:       {:?}\n\
             shell output: {:?}",
            input,
            quoted,
            shell_output
        );
    }
}
