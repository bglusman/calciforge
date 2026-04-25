// Each test in this module mutates process-global env (PATH, secret-name
// vars) and must serialize. The ENV_MUTEX is a sync Mutex held across
// `.await` calls deliberately — we want the env state stable through the
// full `get_secret` call, and each `#[tokio::test]` uses its own
// single-threaded runtime so cross-task contention is not possible.
#![allow(clippy::await_holding_lock)]

//! Adversarial integration tests for the env → fnox → vaultwarden resolver.
//!
//! These tests correspond to T4 in `docs/rfcs/agent-secret-gateway.md`:
//! the architecture claims the resolver falls through gracefully across
//! three layers. This file tries to break that claim.
//!
//! Strategy: we can't easily mock `reqwest` calls to vaultwarden from
//! inside this async function, but we *can* mock `fnox` by putting a
//! fake shell script earlier on PATH. The vault.rs code shells out via
//! `Command::new("fnox")`, which resolves against the child process's
//! PATH — which inherits from our test process env.
//!
//! Each test installs a fake `fnox` binary that behaves a specific way,
//! overrides PATH, then asserts the resolver does the right thing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that mutate the process-global environment (PATH,
/// specific env vars). Required because `std::env::set_var` is
/// process-wide and Cargo runs tests concurrently within a crate.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Wrapper for `std::env::set_var` — Rust 2024 marks it unsafe since
/// it's not thread-safe. Our ENV_MUTEX makes it serialized, so the
/// unsafe is narrowly justified here.
fn set_env<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    // Safety: all call sites hold ENV_MUTEX for the duration of the
    // mutation + any reads, serializing env access.
    unsafe { std::env::set_var(key, value) }
}

fn remove_env<K: AsRef<std::ffi::OsStr>>(key: K) {
    // Safety: same as set_env — ENV_MUTEX serializes.
    unsafe { std::env::remove_var(key) }
}

/// Install a fake `fnox` binary in `dir` that runs `script` as its body.
/// The script receives fnox's argv on stdin via $@. Returns the dir path.
fn install_fake_fnox(dir: &TempDir, body: &str) -> PathBuf {
    let bin = dir.path().join("fnox");
    let content = format!("#!/bin/sh\n{}\n", body);
    fs::write(&bin, content).expect("write fake fnox");
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod fake fnox");
    dir.path().to_path_buf()
}

/// Prepend `dir` to PATH, returning a guard that restores the original
/// PATH on drop. Caller must hold ENV_MUTEX for the duration.
struct PathGuard {
    original: Option<String>,
}

impl PathGuard {
    fn prepend(dir: &std::path::Path) -> Self {
        let original = std::env::var("PATH").ok();
        let new_path = match &original {
            Some(p) => format!("{}:{}", dir.display(), p),
            None => dir.display().to_string(),
        };
        // Also isolate from any pre-set vaultwarden creds so we don't
        // accidentally hit a real server during unit tests.
        set_env("PATH", new_path);
        remove_env("SECRETS_VAULT_TOKEN");
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(p) => set_env("PATH", p),
            None => remove_env("PATH"),
        }
    }
}

/// T4-precondition: env takes precedence over fnox.
///
/// Naming convention: vault.rs transforms the logical name into
/// `{NAME_UPPER}_API_KEY` for env lookup. Tests must use that form.
/// This is an intentional restriction (env fallback is for API-key-style
/// secrets only); if you want arbitrary names in env, use fnox or vault.
///
/// Failure mode this catches: if the env-check drifted after the
/// fnox-check during a refactor, a rotated env key would be ignored in
/// favour of a stale fnox value.
#[tokio::test]
async fn env_var_wins_over_fnox() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let dir = TempDir::new().unwrap();
    let fake_dir = install_fake_fnox(&dir, r#"echo "from-fnox""#);
    let _path_guard = PathGuard::prepend(&fake_dir);

    // vault.rs looks up "{NAME}_API_KEY" when NAME is the logical secret.
    // We pass "t4_envwins" → it will look up "T4_ENVWINS_API_KEY".
    set_env("T4_ENVWINS_API_KEY", "from-env");

    let result = secrets_client::vault::get_secret("t4_envwins").await;

    remove_env("T4_ENVWINS_API_KEY");

    assert!(
        result.is_ok(),
        "resolver should succeed when env is set, got: {:?}",
        result.err()
    );
    assert_eq!(
        result.unwrap(),
        "from-env",
        "env var should win over fnox; if this failed, precedence regressed"
    );
}

/// Documents the env-lookup naming convention as a regression guard.
/// If vault.rs changes the transform, this test forces an intentional
/// update rather than a silent breakage.
#[tokio::test]
async fn env_lookup_uses_uppercase_api_key_suffix() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let dir = TempDir::new().unwrap();
    // No fnox fallback — assert the env path alone works.
    let fake_dir = install_fake_fnox(&dir, r#"exit 5"#);
    let _path_guard = PathGuard::prepend(&fake_dir);

    // Lower-case request should hit UPPERCASE env var.
    set_env("MIXED_CaSE_API_KEY", "wrong"); // wrong case, must be ignored
    set_env("MIXED_CASE_API_KEY", "expected");

    let result = secrets_client::vault::get_secret("mixed_case").await;

    remove_env("MIXED_CaSE_API_KEY");
    remove_env("MIXED_CASE_API_KEY");

    assert_eq!(
        result.expect("env resolve"),
        "expected",
        "env var name should be <NAME_UPPER>_API_KEY exactly — if this fails, \
         vault.rs changed its env naming convention and callers may break"
    );
}

/// T4a: fnox returns a value when env is unset. Resolver returns that
/// value without consulting vaultwarden.
///
/// Failure mode this catches: a silent regression where the fnox call
/// is skipped (e.g. due to a plumbing error) and we fall through to
/// vaultwarden, burning a network round-trip on every request.
#[tokio::test]
async fn fnox_value_returned_when_env_empty() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let dir = TempDir::new().unwrap();
    // Fake fnox that prints the secret name as its value — lets us
    // assert the shell-out worked and got the right argv.
    let fake_dir = install_fake_fnox(&dir, r#"echo "fnox-value-for-$2""#);
    let _path_guard = PathGuard::prepend(&fake_dir);

    // Belt-and-suspenders: ensure the target env var is not set.
    remove_env("VAULT_T4A_FNOX_WINS");

    let result = secrets_client::vault::get_secret("VAULT_T4A_FNOX_WINS").await;

    assert!(
        result.is_ok(),
        "resolver should succeed from fnox, got: {:?}",
        result.err()
    );
    assert_eq!(
        result.unwrap(),
        "fnox-value-for-VAULT_T4A_FNOX_WINS",
        "fnox output should pass through; check argv and trim behavior"
    );
}

/// T4b: fnox fails → fallthrough to vaultwarden.
///
/// We can't run vaultwarden in a unit test, but we can assert that when
/// env + fnox both yield nothing, the resolver proceeds to the
/// vaultwarden path (which will fail because SECRETS_VAULT_TOKEN is
/// unset) and returns the correct "not found" error.
///
/// Failure mode this catches: resolver swallowing the fnox error silently
/// and returning a stale empty string, or panicking instead of erroring.
#[tokio::test]
async fn fnox_failure_falls_through_to_vault_error() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let dir = TempDir::new().unwrap();
    // fnox exits non-zero — simulates missing secret OR broken fnox.
    let fake_dir = install_fake_fnox(&dir, r#"exit 3"#);
    let _path_guard = PathGuard::prepend(&fake_dir);

    remove_env("VAULT_T4B_MISSING");

    let result = secrets_client::vault::get_secret("VAULT_T4B_MISSING").await;

    assert!(
        result.is_err(),
        "should fail when neither env nor fnox has the secret"
    );
    let err = result.unwrap_err().to_string();
    // The final error should mention the secret was not found anywhere.
    // Don't over-assert on the exact phrase — it may legitimately evolve —
    // just confirm the message names the missing key OR the resolver
    // chain so a user can debug.
    assert!(
        err.contains("VAULT_T4B_MISSING")
            || err.contains("not found")
            || err.contains("No SECRETS_VAULT_TOKEN"),
        "error should be user-actionable, got: {}",
        err
    );
}

/// T4c: fnox binary entirely missing from PATH → same graceful failure
/// as fnox-present-but-failing.
///
/// Failure mode this catches: a panic or unclear error when the fnox
/// binary isn't installed (common fresh-host scenario — this is exactly
/// the case install.sh's ensure_fnox is meant to avoid, but we need the
/// resolver to handle it regardless).
#[tokio::test]
async fn fnox_binary_missing_is_graceful() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    // Empty PATH dir so fnox can't be found anywhere.
    let empty = TempDir::new().unwrap();
    // IMPORTANT: completely replace PATH with the empty dir — prepending
    // would still let the real fnox binary (installed by brew) be found.
    let original = std::env::var("PATH").ok();
    set_env("PATH", empty.path().display().to_string());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("VAULT_T4C_NO_FNOX");

    let result = secrets_client::vault::get_secret("VAULT_T4C_NO_FNOX").await;

    // Restore before asserting so any panic below doesn't corrupt
    // the rest of the test run's PATH.
    match original {
        Some(p) => set_env("PATH", p),
        None => remove_env("PATH"),
    }

    assert!(
        result.is_err(),
        "should fail cleanly when fnox binary is absent and nothing else has the secret"
    );
    // Must not panic; must not hang; error text should be user-actionable.
    let err = result.unwrap_err().to_string();
    assert!(
        !err.is_empty(),
        "error must have a message — empty error strings block debugging"
    );
}

/// T4d: fnox returns empty stdout (success but no value).
///
/// Failure mode this catches: a silent regression where an empty
/// string is accepted as a valid "secret" and passed to the upstream
/// auth header — resulting in `Authorization: Bearer ` with nothing
/// after the space, which many providers accept as "anonymous" and
/// behave surprisingly.
#[tokio::test]
async fn fnox_empty_output_is_rejected() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let dir = TempDir::new().unwrap();
    // fnox exits 0 with empty stdout — "success" but no value.
    let fake_dir = install_fake_fnox(&dir, r#"exit 0"#);
    let _path_guard = PathGuard::prepend(&fake_dir);

    remove_env("VAULT_T4D_EMPTY");

    let result = secrets_client::vault::get_secret("VAULT_T4D_EMPTY").await;

    // Either the resolver rejects empty AS fnox-error (preferred — means
    // fall-through to vaultwarden works), or the whole thing errors out
    // with a clear message. Either way, must NOT return Ok("").
    if let Ok(value) = &result {
        panic!(
            "resolver returned Ok({:?}) from empty fnox output — must reject or fall through",
            value
        );
    }
}
