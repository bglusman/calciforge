//! Library-backed `FnoxClient` — calls into the `fnox` crate directly
//! instead of forking a `fnox` subprocess.
//!
//! Enabled via the `fnox-library` cargo feature. When enabled, callers
//! can construct [`FnoxLibrary`] and use the same `get`/`set`/`list`
//! API as the subprocess [`crate::FnoxClient`]. The wrapper does the
//! same config-loading dance that `fnox`'s own binary does, so calls
//! resolve through whatever providers the local `fnox.toml` declares.
//!
//! ## Why this exists
//!
//! Subprocess mode (the default `FnoxClient`) is robust but has real
//! costs: PATH dependency, fork-per-call latency, argv visibility on
//! shared hosts (mitigated via stdin but still a surface), and
//! brittleness around CLI version drift. The library path eliminates
//! all of those.
//!
//! ## Why this isn't the default
//!
//! The fnox crate pulls ~30 transitive dependencies (AWS SDK, GCP SDK,
//! keyring, age, etc.) and adds ~1m 39s to a cold workspace build.
//! Most consumers of `onecli-client` don't need fnox at all — they get
//! credentials from env or vaultwarden. Hiding library mode behind a
//! cargo feature keeps the workspace lean for them.
//!
//! ## Upstream PR
//!
//! The "config-loading dance" replicated below is exactly what fnox's
//! own `GetCommand::run` and `SetCommand::run` do. Filing an upstream
//! PR proposing a top-level `Fnox::discover()` → `fnox.get(name)` API
//! would let downstream library consumers stop replicating this.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(feature = "fnox-library")]
use tracing::debug;

use crate::fnox_client::FnoxError;

const DEFAULT_PROFILE: &str = "default";

/// Library-backed fnox client. Constructed once per process; cheap to
/// clone (just a [`PathBuf`] + [`Duration`]).
///
/// Only useful when built with `--features fnox-library` — the
/// subprocess-only build keeps the type for surface stability but its
/// methods return [`FnoxError::NotInstalled`].
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "fnox-library"), allow(dead_code))]
pub struct FnoxLibrary {
    /// Path to the fnox.toml the binary would discover, OR a directory
    /// to start the upward search from. Discovered lazily per call so
    /// edits to fnox.toml are picked up without a process restart
    /// (matches the binary's behavior).
    config_root: PathBuf,
    /// Profile name to use when resolving secrets. Matches fnox's
    /// `--profile` flag.
    profile: String,
    /// Bound for any single library call. Library calls aren't supposed
    /// to block on remote backends without a deadline, so we wrap each
    /// one in `tokio::time::timeout`.
    timeout: Duration,
}

impl FnoxLibrary {
    /// Construct a library-backed client. `config_root` should be the
    /// directory you'd run `fnox` from — typically the project root or
    /// `$HOME`. Use `FnoxLibrary::discover()` for the same upward-
    /// search semantics fnox's binary uses.
    pub fn new(config_root: impl Into<PathBuf>) -> Self {
        Self {
            config_root: config_root.into(),
            profile: DEFAULT_PROFILE.to_string(),
            timeout: Duration::from_secs(10),
        }
    }

    /// Walk up from `start` looking for a `fnox.toml`, mirroring the
    /// upward-search the binary does. Falls back to `start` itself if
    /// no config is found — fnox's `Config::load_smart` will then emit
    /// the same "no config" error the binary would.
    pub fn discover(start: impl AsRef<Path>) -> Self {
        let mut current = start.as_ref().to_path_buf();
        loop {
            if current.join("fnox.toml").exists() {
                return Self::new(current);
            }
            if !current.pop() {
                // Hit root with no config — let load fail with fnox's
                // own "no config" message rather than make one up.
                return Self::new(start.as_ref());
            }
        }
    }

    /// Override the profile (default: `"default"`).
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
        self
    }

    /// Override the per-call timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Resolve a secret by name. Returns the resolved value, or
    /// [`FnoxError::EmptyValue`] if the configured backend has no
    /// value for the key.
    ///
    /// Behavior: replicates `GetCommand::run` minus the println — load
    /// config, find the secret config for `name` in the active profile,
    /// call `secret_resolver::resolve_secret`.
    #[cfg(feature = "fnox-library")]
    pub async fn get(&self, name: &str) -> Result<String, FnoxError> {
        debug!("fnox-lib get {} (profile={})", name, self.profile);
        let config_path = self.config_root.join("fnox.toml");
        let config = fnox::config::Config::load_smart(&config_path).map_err(map_fnox_err)?;
        let secrets = config.get_secrets(&self.profile).map_err(map_fnox_err)?;
        let secret_config = secrets.get(name).ok_or_else(|| FnoxError::Failed {
            exit_code: None,
            stderr: format!("secret '{name}' not declared in profile '{}'", self.profile),
        })?;

        let result = tokio::time::timeout(
            self.timeout,
            fnox::secret_resolver::resolve_secret(&config, &self.profile, name, secret_config),
        )
        .await
        .map_err(|_| FnoxError::TimedOut {
            seconds: self.timeout.as_secs(),
        })?
        .map_err(map_fnox_err)?;

        match result {
            Some(v) if !v.is_empty() => Ok(v),
            Some(_) => Err(FnoxError::EmptyValue(name.to_string())),
            None => Err(FnoxError::EmptyValue(name.to_string())),
        }
    }

    /// Stub-only when library feature is off. Prevents the type from
    /// being constructable on accident in subprocess-only builds.
    #[cfg(not(feature = "fnox-library"))]
    pub async fn get(&self, _name: &str) -> Result<String, FnoxError> {
        Err(FnoxError::NotInstalled(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "FnoxLibrary requires the `fnox-library` cargo feature",
        )))
    }

    /// List declared secret names for the active profile. Note: this
    /// is the *declared* set from fnox.toml, not necessarily the set
    /// of secrets that have a current value (some may be `if_missing
    /// = "ignore"`).
    #[cfg(feature = "fnox-library")]
    pub async fn list(&self) -> Result<Vec<String>, FnoxError> {
        debug!("fnox-lib list (profile={})", self.profile);
        let config_path = self.config_root.join("fnox.toml");
        let config = fnox::config::Config::load_smart(&config_path).map_err(map_fnox_err)?;
        let secrets = config.get_secrets(&self.profile).map_err(map_fnox_err)?;
        Ok(secrets.keys().cloned().collect())
    }

    #[cfg(not(feature = "fnox-library"))]
    pub async fn list(&self) -> Result<Vec<String>, FnoxError> {
        Err(FnoxError::NotInstalled(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "FnoxLibrary requires the `fnox-library` cargo feature",
        )))
    }

    // NOTE: `set` is intentionally NOT implemented in this first cut.
    // fnox's `SetCommand::run` is ~100 LOC of provider/encryption logic
    // (pick provider, optionally base64-encode, encrypt via provider,
    // write back to TOML). Replicating it here is the bulk of an
    // upstream PR — easier to land that in fnox itself as a top-level
    // `Fnox::set(name, value)` API and call into it. Until then,
    // callers needing `set` use the subprocess `FnoxClient`.
}

#[cfg(feature = "fnox-library")]
fn map_fnox_err(e: fnox::error::FnoxError) -> FnoxError {
    // Coerce to FnoxError::Failed so callers don't need to know the
    // upstream variant set. exit_code is None because no subprocess.
    FnoxError::Failed {
        exit_code: None,
        stderr: e.to_string(),
    }
}
