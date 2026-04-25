//! `FnoxClient` — single-source-of-truth wrapper around the `fnox`
//! CLI subprocess.
//!
//! ## Why this exists
//!
//! Three call-sites in this workspace shell out to `fnox`:
//!
//! - `vault.rs::get_secret` (read path, hot)
//! - `commands.rs::secure_set` (write path, `!secure set`)
//! - `commands.rs::secure_list` (enumerate names, `!secure list`)
//!
//! Pre-this-module each call-site had its own `Command::new("fnox")`,
//! its own stderr-to-string error, and its own
//! "is-fnox-installed?" check. That's three near-identical
//! implementations, three subtly-different error messages, and three
//! places to keep in sync if fnox's CLI ever changes.
//!
//! `FnoxClient` consolidates all of it: typed errors, one PATH check,
//! one set of subprocess plumbing, one fake-fnox testing scaffold.
//! Callers shrink to one-liners.
//!
//! ## Why subprocess (still)
//!
//! Investigated the fnox library crate
//! (`fnox = { git = "https://github.com/jdx/fnox" }`) for direct
//! programmatic access. Findings (2026-04-24):
//!
//! - `fnox::secret_resolver::resolve_secret` exists but requires
//!   pre-loaded `Config` + per-secret `SecretConfig` — it's CLI-style
//!   "I'm building another fnox" use, not "I just want a programmatic
//!   `get(name)`". Caller has to do the config-loading dance fnox's
//!   own binary does for it.
//! - No clean library entry for SET. The CLI's `SetCommand::run` is
//!   `pub` but reads stdin, prints stdout, may exit. Reusing it is
//!   hacky.
//! - Library deps include AWS SDK, GCP SDK, keyring, etc. — ~30
//!   transitive crates, big build-time hit, mostly unused by us.
//!
//! Subprocess is the right interface for our usage today. If fnox
//! grows a clean programmatic API, swap the implementation here
//! without changing callers.
//!
//! ## Testing
//!
//! Tests use `FnoxClient::with_binary(path_to_script)` to point at a
//! shell script that pretends to be fnox. No PATH manipulation, no
//! global env mutation, no `serial_test` requirement.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

const DEFAULT_FNOX_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors from `FnoxClient` operations. Variants are intentionally
/// narrow so callers can pattern-match instead of substring-matching
/// stderr.
#[derive(Debug, Error)]
pub enum FnoxError {
    /// The fnox binary couldn't be executed — typically because it's
    /// not installed or not on `PATH`. Distinct from other failures
    /// because the right action is "install fnox" not "investigate".
    #[error("fnox binary not available (install: brew install fnox; cargo install fnox): {0}")]
    NotInstalled(std::io::Error),

    /// fnox ran successfully but returned an empty value for a secret
    /// that the caller expected to be populated. Treating empty as a
    /// "not found" signal — sending an empty value upstream as
    /// `Authorization: Bearer ` would silently authenticate as
    /// anonymous, which is the worst kind of silent failure.
    #[error("fnox returned empty value for {0:?}")]
    EmptyValue(String),

    /// fnox exited non-zero. `stderr` carries fnox's own error message.
    /// Callers may inspect `stderr` for fine-grained handling but
    /// generally should surface the message to the operator.
    #[error("fnox failed (exit {exit_code:?}): {stderr}")]
    Failed {
        exit_code: Option<i32>,
        stderr: String,
    },

    /// fnox stdout wasn't valid UTF-8. fnox itself should never emit
    /// this; if it happens we treat it as the same severity as a
    /// non-zero exit.
    #[error("fnox returned non-UTF-8 output")]
    InvalidUtf8,

    /// The fnox process started, but stdin/stdout/stderr handling
    /// failed while communicating with it.
    #[error("fnox I/O failed: {0}")]
    Io(std::io::Error),

    /// fnox started or was startable, but did not complete within the
    /// bounded command window. Treat as retryable by the operator, not
    /// as "not installed".
    #[error("fnox timed out after {seconds}s")]
    TimedOut { seconds: u64 },
}

/// Wrapper around the `fnox` CLI. Cheap to construct (just a path);
/// safe to clone; safe to share across tasks.
#[derive(Debug, Clone)]
pub struct FnoxClient {
    /// Path to the fnox binary. Defaults to `"fnox"` (relies on PATH);
    /// tests override via [`FnoxClient::with_binary`] to point at a
    /// fake shell script.
    binary: PathBuf,
    timeout: Duration,
}

impl Default for FnoxClient {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("fnox"),
            timeout: DEFAULT_FNOX_TIMEOUT,
        }
    }
}

impl FnoxClient {
    /// Construct a client that uses the `fnox` binary on `PATH`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a client that uses a specific binary path. Test
    /// scaffolding uses this to point at a fake shell script;
    /// production code shouldn't need it.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: DEFAULT_FNOX_TIMEOUT,
        }
    }

    /// Construct a client with a custom timeout. Production code uses
    /// the default; tests use this to keep timeout assertions fast.
    pub fn with_binary_and_timeout(binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            timeout,
        }
    }

    /// True if the configured binary can be executed (a `fnox --version`
    /// returned success). Useful for the `!secure` command's first-use
    /// check — we want to give the user a clear "install fnox" message
    /// instead of a confusing failure on the first set.
    pub async fn is_available(&self) -> bool {
        let mut command = Command::new(&self.binary);
        command
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        timeout(self.timeout, command.output())
            .await
            .is_ok_and(|result| result.is_ok_and(|o| o.status.success()))
    }

    /// `fnox get <name>` — return the value, or an error.
    ///
    /// Empty values are rejected ([`FnoxError::EmptyValue`]) — see the
    /// type doc for why.
    pub async fn get(&self, name: &str) -> Result<String, FnoxError> {
        debug!("fnox get {}", name);
        let output = self.run(&["get", name]).await?;

        if !output.status.success() {
            return Err(FnoxError::Failed {
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let value = String::from_utf8(output.stdout)
            .map_err(|_| FnoxError::InvalidUtf8)?
            .trim()
            .to_string();

        if value.is_empty() {
            return Err(FnoxError::EmptyValue(name.to_string()));
        }
        Ok(value)
    }

    /// `fnox set <name>` — store a secret.
    ///
    /// `value` is written to stdin instead of argv. fnox explicitly
    /// supports this form when the value argument is omitted, and it
    /// keeps the secret out of `ps` output and process-monitor logs.
    pub async fn set(&self, name: &str, value: &str) -> Result<(), FnoxError> {
        debug!("fnox set {}", name);
        let mut child = Command::new(&self.binary)
            .args(["set", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(map_spawn_error)?;

        let mut stdin = child.stdin.take().ok_or_else(|| {
            FnoxError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fnox stdin pipe unavailable",
            ))
        })?;
        stdin
            .write_all(value.as_bytes())
            .await
            .map_err(FnoxError::Io)?;
        stdin.shutdown().await.map_err(FnoxError::Io)?;
        drop(stdin);

        let output = timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| FnoxError::TimedOut {
                seconds: self.timeout.as_secs(),
            })?
            .map_err(FnoxError::Io)?;

        if !output.status.success() {
            return Err(FnoxError::Failed {
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(())
    }

    /// `fnox list` — return the list of stored secret NAMES.
    ///
    /// Defensive parse: fnox's CLI output format varies slightly by
    /// version (some versions emit a table, some emit `name value`
    /// pairs). We extract the first whitespace-separated token from
    /// each non-comment, non-empty line and treat it as a name.
    /// Anything else on the line — values, descriptions, table
    /// borders — is dropped. Callers that need richer info should
    /// use `fnox list --output json` directly once we wire that up.
    pub async fn list(&self) -> Result<Vec<String>, FnoxError> {
        debug!("fnox list");
        let output = self.run(&["list"]).await?;

        if !output.status.success() {
            return Err(FnoxError::Failed {
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let names = stdout
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    None
                } else {
                    trimmed.split_whitespace().next().map(String::from)
                }
            })
            .collect();
        Ok(names)
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, FnoxError> {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        timeout(self.timeout, command.output())
            .await
            .map_err(|_| FnoxError::TimedOut {
                seconds: self.timeout.as_secs(),
            })?
            .map_err(map_spawn_error)
    }
}

fn map_spawn_error(error: std::io::Error) -> FnoxError {
    if error.kind() == std::io::ErrorKind::NotFound {
        FnoxError::NotInstalled(error)
    } else {
        FnoxError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    //! Tests use `FnoxClient::with_binary(path)` pointing at a shell
    //! script that pretends to be fnox. No PATH manipulation, no
    //! global env mutation, no need for serial test execution.

    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Build a fake fnox at `dir/fnox` whose body is `script`. Returns
    /// the path so the test can hand it to `FnoxClient::with_binary`.
    fn fake_fnox(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("fnox");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    /// Given a fake fnox that prints a value,
    /// when `get(NAME)` is called,
    /// then the returned value matches what fnox printed.
    #[tokio::test]
    async fn get_returns_value_on_success() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, r#"echo "the-value""#);
        let client = FnoxClient::with_binary(bin);

        let result = client.get("ANYTHING").await;
        assert_eq!(result.unwrap(), "the-value");
    }

    /// Given a fake fnox that prints a value with trailing newline,
    /// when get is called,
    /// then the returned string is trimmed.
    #[tokio::test]
    async fn get_trims_trailing_whitespace() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, r#"printf "value-with-newline\n\n""#);
        let client = FnoxClient::with_binary(bin);

        assert_eq!(client.get("X").await.unwrap(), "value-with-newline");
    }

    /// Given a fake fnox that exits 0 with empty stdout,
    /// when get is called,
    /// then the result is `Err(EmptyValue(name))` — rejecting empty
    /// rather than returning `Ok("")` which would silently auth as
    /// anonymous when used in a Bearer header.
    #[tokio::test]
    async fn get_empty_value_is_error_not_empty_string() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "exit 0");
        let client = FnoxClient::with_binary(bin);

        let err = client.get("EMPTY_KEY").await.unwrap_err();
        assert!(
            matches!(err, FnoxError::EmptyValue(ref n) if n == "EMPTY_KEY"),
            "expected EmptyValue(EMPTY_KEY), got {err:?}"
        );
    }

    /// Given a fake fnox that exits non-zero,
    /// when get is called,
    /// then the result is `Err(Failed { exit_code, stderr })` carrying
    /// fnox's own diagnostic text — callers can surface that to the
    /// operator without the wrapper inventing its own message.
    #[tokio::test]
    async fn get_failure_carries_exit_code_and_stderr() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, r#"echo "key 'X' not defined" >&2; exit 7"#);
        let client = FnoxClient::with_binary(bin);

        let err = client.get("X").await.unwrap_err();
        match err {
            FnoxError::Failed { exit_code, stderr } => {
                assert_eq!(exit_code, Some(7));
                assert!(
                    stderr.contains("not defined"),
                    "stderr should propagate: got {stderr:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Given a binary path that doesn't exist,
    /// when get is called,
    /// then the result is `Err(NotInstalled(_))` — distinct error so
    /// callers (e.g. `!secure`) can give a "install fnox" hint.
    #[tokio::test]
    async fn get_returns_not_installed_when_binary_missing() {
        let client = FnoxClient::with_binary("/tmp/definitely-not-a-real-binary-pid-zzz");

        let err = client.get("X").await.unwrap_err();
        assert!(
            matches!(err, FnoxError::NotInstalled(_)),
            "expected NotInstalled, got {err:?}"
        );
    }

    /// Given a binary path that exists but cannot be executed,
    /// when get is called,
    /// then the wrapper reports an I/O error instead of implying fnox
    /// is missing from PATH.
    #[tokio::test]
    async fn get_permission_denied_is_io_not_not_installed() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("fnox");
        fs::write(&bin, "#!/bin/sh\necho nope\n").unwrap();
        let client = FnoxClient::with_binary(bin);

        let err = client.get("X").await.unwrap_err();
        assert!(matches!(err, FnoxError::Io(_)), "expected Io, got {err:?}");
    }

    /// Given a fake fnox that succeeds silently for `set`,
    /// when set is called,
    /// then the result is Ok(()).
    #[tokio::test]
    async fn set_succeeds_when_fnox_succeeds() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "cat >/dev/null; exit 0");
        let client = FnoxClient::with_binary(bin);

        client.set("MY_KEY", "my-value").await.unwrap();
    }

    /// Given a fake fnox that captures argv and stdin,
    /// when set("KEY", "value") is called,
    /// then the captured argv contains the operation and name while
    /// the value arrives over stdin.
    #[tokio::test]
    async fn set_passes_value_over_stdin_not_argv() {
        let dir = TempDir::new().unwrap();
        let argv_log = dir.path().join("argv.log");
        let stdin_log = dir.path().join("stdin.log");
        let bin = fake_fnox(
            &dir,
            &format!(
                r#"printf '%s\n' "$@" > {} ; cat > {} ; exit 0"#,
                argv_log.display(),
                stdin_log.display()
            ),
        );
        let client = FnoxClient::with_binary(bin);

        client.set("MY_KEY", "the-value").await.unwrap();

        let argv = fs::read_to_string(&argv_log).expect("argv.log written");
        assert_eq!(argv.trim(), "set\nMY_KEY");
        assert!(
            !argv.contains("the-value"),
            "argv must not contain the secret value: {argv:?}"
        );

        let stdin = fs::read_to_string(&stdin_log).expect("stdin.log written");
        assert_eq!(stdin, "the-value");
    }

    /// Given a fake fnox that hangs,
    /// when get is called with a short timeout,
    /// then the wrapper returns TimedOut instead of blocking forever.
    #[tokio::test]
    async fn get_times_out_when_fnox_hangs() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "sleep 5");
        let client = FnoxClient::with_binary_and_timeout(bin, Duration::from_millis(25));

        let err = client.get("SLOW_KEY").await.unwrap_err();
        assert!(
            matches!(err, FnoxError::TimedOut { .. }),
            "expected TimedOut, got {err:?}"
        );
    }

    /// Given a fake fnox that hangs while reading a set request,
    /// when set is called with a short timeout,
    /// then the wrapper returns TimedOut instead of blocking forever.
    #[tokio::test]
    async fn set_times_out_when_fnox_hangs() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "sleep 5");
        let client = FnoxClient::with_binary_and_timeout(bin, Duration::from_millis(25));

        let err = client.set("SLOW_KEY", "value").await.unwrap_err();
        assert!(
            matches!(err, FnoxError::TimedOut { .. }),
            "expected TimedOut, got {err:?}"
        );
    }

    /// Given a fake fnox that prints `name value` pairs on `list`,
    /// when list is called,
    /// then only the names are returned, value columns dropped.
    #[tokio::test]
    async fn list_extracts_names_only_dropping_value_columns() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"cat <<OUT
KEY_A  redacted-value-A
KEY_B  redacted-value-B
KEY_C  redacted-value-C
OUT"#,
        );
        let client = FnoxClient::with_binary(bin);

        let names = client.list().await.unwrap();
        assert_eq!(names, vec!["KEY_A", "KEY_B", "KEY_C"]);
        // Defensive: we don't trust the wrapper not to leak; assert
        // explicitly that no value substring survived.
        for v in ["redacted-value-A", "redacted-value-B", "redacted-value-C"] {
            assert!(
                !names.iter().any(|n| n.contains(v)),
                "list must not surface value column data: {names:?}"
            );
        }
    }

    /// Given a fake fnox whose `list` output mixes blank lines and
    /// `# comment` lines among real names,
    /// when list is called,
    /// then comments and blanks are filtered out.
    #[tokio::test]
    async fn list_filters_blank_and_comment_lines() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"cat <<OUT

# header line
ONE_KEY  v
# another comment
TWO_KEY  v

OUT"#,
        );
        let client = FnoxClient::with_binary(bin);

        let names = client.list().await.unwrap();
        assert_eq!(names, vec!["ONE_KEY", "TWO_KEY"]);
    }

    /// Given a real fake fnox that runs `--version` on `is_available`,
    /// when called against a missing path,
    /// then `is_available` returns false (not panic, not block).
    #[tokio::test]
    async fn is_available_false_for_missing_binary() {
        let client = FnoxClient::with_binary("/tmp/no-such-fnox-pid-xyz");
        assert!(!client.is_available().await);
    }

    /// And true for a binary that exits 0 on --version.
    #[tokio::test]
    async fn is_available_true_for_binary_that_succeeds() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "exit 0");
        let client = FnoxClient::with_binary(bin);
        assert!(client.is_available().await);
    }
}
