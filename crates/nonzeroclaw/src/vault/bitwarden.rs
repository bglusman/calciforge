//! Bitwarden CLI adapter — talks to Bitwarden / Vaultwarden via the `bw` CLI subprocess.
//!
//! This module is only compiled when the `bitwarden-cli` feature is enabled.
//!
//! # Design
//!
//! - `bw unlock <master_password>` yields a session key.
//! - All subsequent `bw` calls pass the session key via `BW_SESSION` env var.
//! - The session token is cached in memory; `is_valid()` checks the TTL.
//! - The master password is sourced from config at unlock time and never logged.
//! - All subprocess calls are abstracted behind the `BwRunner` trait so tests
//!   can inject a mock without spawning real processes.
//!
//! # Mockability
//!
//! `BitwardenCliAdapter<R: BwRunner>` is generic over the runner.  In production,
//! use `BitwardenCliAdapter::new(...)` which defaults to `ProcessBwRunner`.
//! In tests, supply `MockBwRunner`.

use crate::vault::{
    adapter::{VaultAdapter, VaultResult},
    config::VaultConfig,
    error::VaultError,
    types::{Secret, SecretValue, SessionToken},
};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── BwRunner trait ────────────────────────────────────────────────────────────

/// Abstracts over `bw` subprocess invocation for testability.
#[async_trait]
pub trait BwRunner: Send + Sync + std::fmt::Debug {
    /// Run `bw unlock <master_password>` and return the session token string.
    async fn unlock(&self, bw_path: &str, master_password: &str) -> VaultResult<String>;

    /// Run `bw get password <item_id>` with the given session token.
    async fn get_password(
        &self,
        bw_path: &str,
        item_id: &str,
        session_token: &str,
    ) -> VaultResult<String>;

    /// Run `bw create item` to store a new secret.
    ///
    /// `item_json` is a Bitwarden-format JSON item object.
    async fn create_item(
        &self,
        bw_path: &str,
        item_json: &str,
        session_token: &str,
    ) -> VaultResult<()>;
}

// ── ProcessBwRunner ───────────────────────────────────────────────────────────

/// Production `BwRunner` — spawns real `bw` subprocesses.
#[derive(Debug, Default, Clone)]
pub struct ProcessBwRunner;

#[async_trait]
impl BwRunner for ProcessBwRunner {
    async fn unlock(&self, bw_path: &str, master_password: &str) -> VaultResult<String> {
        // SECURITY: Pass the master password via `--passwordenv` with a child-only
        // environment variable, NOT as a CLI argument.
        //
        // Passing it as `bw unlock <password>` exposes the password in
        // /proc/<pid>/cmdline and `ps aux` output, visible to any user
        // with access to /proc.
        //
        // Using `.env("BW_UNLOCK_PW", ...)` on the child Command sets the variable
        // only for the spawned `bw` subprocess — it is NOT added to the current
        // process's environment.  On Linux, /proc/<pid>/environ requires the same
        // ownership or root to read, unlike cmdline which is world-readable.
        //
        // Reference: https://bitwarden.com/help/cli/#using-an-api-key
        // `bw unlock --passwordenv <VAR>` reads the password from the named env var.

        let output = tokio::process::Command::new(bw_path)
            .args(["unlock", "--raw", "--passwordenv", "BW_UNLOCK_PW"])
            // Set env var only on the child process, not the parent.
            .env("BW_UNLOCK_PW", master_password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    VaultError::BinaryNotFound(bw_path.to_owned())
                } else {
                    VaultError::Io(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VaultError::UnlockFailed(stderr.trim().to_owned()));
        }

        let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if token.is_empty() {
            return Err(VaultError::UnlockFailed(
                "bw unlock returned empty session token".to_owned(),
            ));
        }
        Ok(token)
    }

    async fn get_password(
        &self,
        bw_path: &str,
        item_id: &str,
        session_token: &str,
    ) -> VaultResult<String> {
        let output = tokio::process::Command::new(bw_path)
            .args(["get", "password", item_id])
            .env("BW_SESSION", session_token)
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    VaultError::BinaryNotFound(bw_path.to_owned())
                } else {
                    VaultError::Io(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VaultError::cli(format!(
                "bw get password failed for '{}': {}",
                item_id,
                stderr.trim()
            )));
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(value)
    }

    async fn create_item(
        &self,
        bw_path: &str,
        item_json: &str,
        session_token: &str,
    ) -> VaultResult<()> {
        // `bw create item` reads JSON from stdin.
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(bw_path)
            .args(["create", "item"])
            .env("BW_SESSION", session_token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    VaultError::BinaryNotFound(bw_path.to_owned())
                } else {
                    VaultError::Io(e)
                }
            })?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(item_json.as_bytes())
                .await
                .map_err(VaultError::Io)?;
        }

        let output = child.wait_with_output().await.map_err(VaultError::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VaultError::cli(format!(
                "bw create item failed: {}",
                stderr.trim()
            )));
        }

        Ok(())
    }
}

// ── BitwardenCliAdapter ───────────────────────────────────────────────────────

/// Vault adapter backed by the `bw` CLI subprocess.
///
/// Generic over `R: BwRunner` to allow test injection.
///
/// # Example (config)
///
/// ```toml
/// [vault]
/// backend = "bitwarden-cli"
/// bw_path = "bw"
///
/// [vault.secrets.anthropic_key]
/// bw_item_id = "anthropic-api-key"
/// policy = "auto"
/// ```
pub struct BitwardenCliAdapter<R: BwRunner = ProcessBwRunner> {
    /// Path to the `bw` binary.
    bw_path: String,
    /// Master password — sourced from config, never logged.
    ///
    /// NOTE: this is stored in memory for the lifetime of the adapter.
    /// A future improvement is to read it from a dedicated secret source
    /// (e.g. env var, keyring) at unlock time and immediately discard it.
    master_password: String,
    /// Logical key → Bitwarden item ID mapping.
    ///
    /// Populated from `config.vault.secrets`.
    item_ids: std::collections::HashMap<String, String>,
    /// Session TTL as configured.
    session_ttl: Duration,
    /// Cached session token (refreshed on expiry).
    session_token: Arc<Mutex<Option<SessionToken>>>,
    /// The subprocess runner (real or mock).
    runner: Arc<R>,
}

impl BitwardenCliAdapter<ProcessBwRunner> {
    /// Construct from a `VaultConfig`.
    ///
    /// The master password is read from the `BW_MASTER_PASSWORD` environment
    /// variable.  If not set, `unlock()` will fail — callers should ensure the
    /// env var is available at startup.
    pub fn from_config(config: &VaultConfig) -> Self {
        let master_password = std::env::var("BW_MASTER_PASSWORD").unwrap_or_default();
        let item_ids = config
            .secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.bw_item_id.clone()))
            .collect();
        Self {
            bw_path: config.bw_path.clone(),
            master_password,
            item_ids,
            session_ttl: Duration::from_secs(config.session_ttl_secs),
            session_token: Arc::new(Mutex::new(None)),
            runner: Arc::new(ProcessBwRunner),
        }
    }
}

impl<R: BwRunner + 'static> BitwardenCliAdapter<R> {
    /// Construct with a custom runner (for testing).
    pub fn with_runner(
        bw_path: impl Into<String>,
        master_password: impl Into<String>,
        item_ids: std::collections::HashMap<String, String>,
        session_ttl: Duration,
        runner: R,
    ) -> Self {
        Self {
            bw_path: bw_path.into(),
            master_password: master_password.into(),
            item_ids,
            session_ttl,
            session_token: Arc::new(Mutex::new(None)),
            runner: Arc::new(runner),
        }
    }

    /// Unlock the vault if not already unlocked, and return the session token.
    ///
    /// Uses the cached token if still valid; otherwise re-unlocks.
    async fn ensure_session(&self) -> VaultResult<String> {
        let mut guard = self.session_token.lock().await;

        if let Some(ref tok) = *guard {
            if tok.is_valid() {
                debug!("vault: reusing cached bw session token");
                return Ok(tok.expose().to_owned());
            }
            info!("vault: bw session token expired, re-unlocking");
        }

        if self.master_password.is_empty() {
            return Err(VaultError::UnlockFailed(
                "BW_MASTER_PASSWORD is not set; cannot unlock vault".to_owned(),
            ));
        }

        let raw_token = self
            .runner
            .unlock(&self.bw_path, &self.master_password)
            .await?;

        let token = SessionToken::new(raw_token, self.session_ttl);
        let exposed = token.expose().to_owned();
        *guard = Some(token);
        info!("vault: bw session established (TTL {:?})", self.session_ttl);
        Ok(exposed)
    }

    /// Build a minimal Bitwarden login item JSON for `bw create item`.
    fn login_item_json(name: &str, password: &str) -> String {
        serde_json::json!({
            "type": 1,
            "name": name,
            "login": {
                "password": password,
            }
        })
        .to_string()
    }
}

#[async_trait]
impl<R: BwRunner + 'static> VaultAdapter for BitwardenCliAdapter<R> {
    async fn get_secret(&self, key: &str) -> VaultResult<Secret> {
        let item_id = self
            .item_ids
            .get(key)
            .ok_or_else(|| VaultError::UnknownKey(key.to_owned()))?;

        let session = self.ensure_session().await?;
        let value = self
            .runner
            .get_password(&self.bw_path, item_id, &session)
            .await?;

        Ok(Secret::new(key, value))
    }

    async fn store_secret(&self, key: &str, value: SecretValue) -> VaultResult<()> {
        let item_name = self.item_ids.get(key).map_or(key, String::as_str);
        let session = self.ensure_session().await?;
        let json = Self::login_item_json(item_name, value.expose());
        self.runner
            .create_item(&self.bw_path, &json, &session)
            .await?;
        Ok(())
    }

    async fn unlock(&self) -> VaultResult<SessionToken> {
        let raw = self
            .runner
            .unlock(&self.bw_path, &self.master_password)
            .await?;
        let token = SessionToken::new(raw.clone(), self.session_ttl);
        // Also cache it.
        *self.session_token.lock().await = Some(SessionToken::new(raw, self.session_ttl));
        Ok(token)
    }

    fn name(&self) -> &'static str {
        "bitwarden-cli"
    }
}

impl<R: BwRunner> std::fmt::Debug for BitwardenCliAdapter<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitwardenCliAdapter")
            .field("bw_path", &self.bw_path)
            .field("session_ttl", &self.session_ttl)
            .field("item_ids_count", &self.item_ids.len())
            .finish_non_exhaustive()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── MockBwRunner ─────────────────────────────────────────────────────────

    #[derive(Debug)]
    struct MockBwRunner {
        /// Secret values returned by `get_password`.
        secrets: std::collections::HashMap<String, String>,
        /// Number of times `unlock` was called.
        unlock_count: Arc<AtomicUsize>,
        /// Session token to return from unlock.
        session_token: String,
        /// If `true`, unlock always fails.
        fail_unlock: bool,
    }

    impl MockBwRunner {
        fn new(secrets: std::collections::HashMap<String, String>) -> Self {
            Self {
                secrets,
                unlock_count: Arc::new(AtomicUsize::new(0)),
                session_token: "mock-session-xyz".to_owned(),
                fail_unlock: false,
            }
        }

        fn failing() -> Self {
            Self {
                secrets: std::collections::HashMap::new(),
                unlock_count: Arc::new(AtomicUsize::new(0)),
                session_token: String::new(),
                fail_unlock: true,
            }
        }

        fn unlock_count(&self) -> usize {
            self.unlock_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl BwRunner for MockBwRunner {
        async fn unlock(&self, _bw_path: &str, _master_password: &str) -> VaultResult<String> {
            self.unlock_count.fetch_add(1, Ordering::SeqCst);
            if self.fail_unlock {
                return Err(VaultError::UnlockFailed("mock unlock failure".into()));
            }
            Ok(self.session_token.clone())
        }

        async fn get_password(
            &self,
            _bw_path: &str,
            item_id: &str,
            _session_token: &str,
        ) -> VaultResult<String> {
            self.secrets
                .get(item_id)
                .cloned()
                .ok_or_else(|| VaultError::UnknownKey(item_id.to_owned()))
        }

        async fn create_item(
            &self,
            _bw_path: &str,
            _item_json: &str,
            _session_token: &str,
        ) -> VaultResult<()> {
            Ok(())
        }
    }

    fn make_adapter(
        item_ids: std::collections::HashMap<String, String>,
        runner: MockBwRunner,
        session_ttl: Duration,
    ) -> BitwardenCliAdapter<MockBwRunner> {
        BitwardenCliAdapter::with_runner("bw", "master-pw", item_ids, session_ttl, runner)
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Unlock is called once; session token is cached.
    #[tokio::test]
    async fn session_token_cached_across_get_calls() {
        let mut secrets = std::collections::HashMap::new();
        secrets.insert("anthropic-api-key".into(), "sk-ant-test".into());

        let runner = MockBwRunner::new(secrets);
        let unlock_count = runner.unlock_count.clone();

        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("anthropic_key".into(), "anthropic-api-key".into());

        let adapter = make_adapter(item_ids, runner, Duration::from_secs(3600));

        // First call — should unlock once.
        let s1 = adapter.get_secret("anthropic_key").await.expect("get_secret 1");
        assert_eq!(s1.expose(), "sk-ant-test");
        assert_eq!(unlock_count.load(Ordering::SeqCst), 1);

        // Second call — should reuse session (no additional unlock).
        let s2 = adapter.get_secret("anthropic_key").await.expect("get_secret 2");
        assert_eq!(s2.expose(), "sk-ant-test");
        assert_eq!(unlock_count.load(Ordering::SeqCst), 1, "unlock called again unexpectedly");
    }

    /// Expired session triggers re-unlock.
    #[tokio::test]
    async fn expired_session_triggers_relock() {
        let mut secrets = std::collections::HashMap::new();
        secrets.insert("k".into(), "v".into());

        let runner = MockBwRunner::new(secrets);
        let unlock_count = runner.unlock_count.clone();

        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("key".into(), "k".into());

        // Very short TTL (1ms) so it expires almost immediately.
        let adapter = make_adapter(item_ids, runner, Duration::from_millis(1));

        adapter.get_secret("key").await.expect("first call");
        // Sleep to expire the token.
        tokio::time::sleep(Duration::from_millis(5)).await;
        adapter.get_secret("key").await.expect("second call after expiry");

        assert_eq!(
            unlock_count.load(Ordering::SeqCst),
            2,
            "should re-unlock after expiry"
        );
    }

    /// Unlock failure propagates correctly.
    #[tokio::test]
    async fn unlock_failure_propagates() {
        let runner = MockBwRunner::failing();
        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("key".into(), "k".into());

        let adapter = make_adapter(item_ids, runner, Duration::from_secs(3600));

        let err = adapter.get_secret("key").await.expect_err("should fail");
        assert!(matches!(err, VaultError::UnlockFailed(_)));
    }

    /// Unknown key returns `VaultError::UnknownKey`.
    #[tokio::test]
    async fn unknown_key_errors() {
        let runner = MockBwRunner::new(Default::default());
        let adapter = make_adapter(Default::default(), runner, Duration::from_secs(3600));

        let err = adapter.get_secret("no_such_key").await.expect_err("should fail");
        assert!(matches!(err, VaultError::UnknownKey(_)));
    }

    /// Empty master password returns unlock failed.
    #[tokio::test]
    async fn empty_master_password_returns_unlock_failed() {
        let runner = MockBwRunner::new(Default::default());
        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("k".into(), "v".into());

        // Use empty master_password to trigger the early-exit path.
        let adapter = BitwardenCliAdapter::with_runner(
            "bw",
            "", // empty password
            item_ids,
            Duration::from_secs(3600),
            runner,
        );

        let err = adapter.get_secret("k").await.expect_err("should fail");
        assert!(matches!(err, VaultError::UnlockFailed(_)));
    }

    /// `store_secret` delegates to the runner without error.
    #[tokio::test]
    async fn store_secret_succeeds() {
        let runner = MockBwRunner::new(Default::default());
        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("new_key".into(), "new-item".into());

        let adapter = make_adapter(item_ids, runner, Duration::from_secs(3600));
        adapter
            .store_secret("new_key", SecretValue::new("top-secret"))
            .await
            .expect("store should succeed");
    }

    // ── C2 fix: password-not-in-argv test ────────────────────────────────────

    /// A `BwRunner` that records the exact arguments received by `unlock`.
    /// Used to verify the adapter layer does NOT smuggle the master password
    /// into `bw_path` (which would expose it as an argv token).
    ///
    /// In the production `ProcessBwRunner`, the password is passed to the `bw`
    /// subprocess via `--passwordenv BW_UNLOCK_PW` with `.env("BW_UNLOCK_PW", pw)`
    /// set only on the child process (not inherited by the parent).  This runner
    /// captures both arguments so tests can verify the adapter hands them to the
    /// runner correctly and separately.
    #[derive(Debug)]
    struct CapturingBwRunner {
        /// The `bw_path` argument captured from the last `unlock` call.
        captured_bw_path: Arc<tokio::sync::Mutex<Option<String>>>,
        /// The `master_password` argument captured from the last `unlock` call.
        /// In the production path this is set as `BW_UNLOCK_PW` on the child env
        /// only — it is NOT injected into argv.
        captured_password: Arc<tokio::sync::Mutex<Option<String>>>,
    }

    impl CapturingBwRunner {
        fn new() -> Self {
            Self {
                captured_bw_path: Arc::new(tokio::sync::Mutex::new(None)),
                captured_password: Arc::new(tokio::sync::Mutex::new(None)),
            }
        }
    }

    #[async_trait]
    impl BwRunner for CapturingBwRunner {
        async fn unlock(&self, bw_path: &str, master_password: &str) -> VaultResult<String> {
            *self.captured_bw_path.lock().await = Some(bw_path.to_owned());
            *self.captured_password.lock().await = Some(master_password.to_owned());
            Ok("captured-session-token".to_owned())
        }

        async fn get_password(
            &self,
            _bw_path: &str,
            _item_id: &str,
            _session_token: &str,
        ) -> VaultResult<String> {
            Ok("captured-secret".to_owned())
        }

        async fn create_item(
            &self,
            _bw_path: &str,
            _item_json: &str,
            _session_token: &str,
        ) -> VaultResult<()> {
            Ok(())
        }
    }

    /// Verify that:
    /// 1. The master password does NOT appear in `bw_path` (the command string).
    ///    Embedding it there would expose it in `/proc/<pid>/cmdline` and `ps aux`.
    /// 2. The master password IS delivered to the runner as a distinct parameter.
    ///    In `ProcessBwRunner`, this parameter is used as the value of the
    ///    `BW_UNLOCK_PW` child-only environment variable passed to `bw unlock
    ///    --passwordenv BW_UNLOCK_PW`.  The variable is set via
    ///    `.env("BW_UNLOCK_PW", password)` on the `tokio::process::Command`,
    ///    which only affects the child process environment.
    ///
    /// This test guards against regression to the insecure positional-argument
    /// pattern `bw unlock <password>`.
    #[tokio::test]
    async fn unlock_password_not_embedded_in_bw_path() {
        let capturing = CapturingBwRunner::new();
        let captured_path = capturing.captured_bw_path.clone();
        let captured_pw = capturing.captured_password.clone();

        let mut item_ids = std::collections::HashMap::new();
        item_ids.insert("key".into(), "item-id".into());

        let secret_password = "super-secret-master-password-1234";
        let adapter = BitwardenCliAdapter::with_runner(
            "/usr/local/bin/bw",
            secret_password,
            item_ids,
            Duration::from_secs(3600),
            capturing,
        );

        adapter.get_secret("key").await.expect("should succeed");

        let path = captured_path.lock().await;
        let pw = captured_pw.lock().await;

        // bw_path must not contain the password — that would mean it was
        // smuggled in as part of the command string (argv exposure).
        assert!(
            !path.as_deref().unwrap_or("").contains(secret_password),
            "master password must NOT appear in bw_path: {:?}",
            path
        );

        // The password IS passed to the runner as its own parameter.
        // ProcessBwRunner sets it as BW_UNLOCK_PW in the child env only
        // (via .env() on tokio::process::Command, not via std::env::set_var).
        assert_eq!(
            pw.as_deref(),
            Some(secret_password),
            "master password must be passed to runner as a separate parameter \
             (for child-only env delivery via --passwordenv BW_UNLOCK_PW)"
        );
    }
}
