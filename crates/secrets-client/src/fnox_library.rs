//! Library-backed fnox client — calls into the upstream `fnox::Fnox`
//! convenience API instead of forking a `fnox` subprocess.
//!
//! Enabled via the `fnox-library` cargo feature. When enabled, callers
//! construct [`FnoxLibrary`] and use the same `get`/`list` API as the
//! subprocess [`crate::FnoxClient`]. Resolution goes through whatever
//! providers the local `fnox.toml` (and merged parent + local + global
//! configs) declare.
//!
//! ## Why this exists
//!
//! Subprocess mode (the default [`crate::FnoxClient`]) is robust but
//! has real costs: PATH dependency, fork-per-call latency, argv
//! visibility on shared hosts (mitigated via stdin but still a
//! surface), and brittleness around CLI version drift. The library
//! path eliminates all of those.
//!
//! ## Why this isn't the default
//!
//! The fnox crate pulls ~30 transitive dependencies (AWS SDK, GCP SDK,
//! keyring, age, etc.) and adds ~1m 39s to a cold workspace build.
//! Most consumers of `secrets-client` don't need fnox at all — they get
//! credentials from env or vaultwarden. Hiding library mode behind a
//! cargo feature keeps the workspace lean for them.
//!
//! ## Upstream
//!
//! Earlier versions of this file replicated `fnox`'s own binary
//! orchestration (config loading, profile resolution, secret-config
//! lookup, resolver invocation). That replication is now upstream as
//! [`fnox::Fnox`] (PR #442 against jdx/fnox) so this wrapper collapses
//! to a thin error-coercion shim. Until that PR lands and ships in a
//! crates.io release, the workspace points at our fork branch via
//! `Cargo.toml`.

use std::path::PathBuf;

use crate::fnox_client::FnoxError;

/// Library-backed fnox client. Cheap to construct (records the
/// directory the user expects fnox.toml to live in or below); cheaper
/// to clone (no Config held until first call).
///
/// Only useful when built with `--features fnox-library` — the
/// subprocess-only build keeps the type for surface stability but its
/// methods return a clear "feature not enabled" error.
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(feature = "fnox-library"), allow(dead_code))]
pub struct FnoxLibrary {
    /// Working directory the upstream `fnox::Fnox::discover()` should
    /// search from. Held lazily — config is loaded fresh per call so
    /// edits to fnox.toml are picked up without restarting (matches
    /// binary behavior). For `discover()` we use the process's CWD;
    /// for `with_root()` callers can pin a specific dir.
    root: Option<PathBuf>,
    /// Optional profile override; None defers to upstream's
    /// `Config::get_profile(None)` (honors `FNOX_PROFILE`).
    profile: Option<String>,
}

impl FnoxLibrary {
    /// Use upstream `fnox::Fnox::discover()` semantics — walk up from
    /// CWD looking for fnox.toml, merge in local-overrides + parent +
    /// global configs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin to a specific directory. Tests and daemons running outside
    /// the project tree use this; production code typically wants
    /// [`FnoxLibrary::new`] with the binary's discovery semantics.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Override profile. None defers to `FNOX_PROFILE` resolution.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    /// Resolve a secret value via fnox's library API.
    /// Returns [`FnoxError::EmptyValue`] when the secret resolves to
    /// `None` or an empty string (consistent with subprocess
    /// `FnoxClient::get`).
    #[cfg(feature = "fnox-library")]
    pub async fn get(&self, name: &str) -> Result<String, FnoxError> {
        let fnox = self.build()?;
        match fnox.get(name).await.map_err(map_fnox_err)? {
            Some(v) if !v.is_empty() => Ok(v),
            _ => Err(FnoxError::EmptyValue(name.to_string())),
        }
    }

    /// List declared secret names for the active profile.
    #[cfg(feature = "fnox-library")]
    pub async fn list(&self) -> Result<Vec<String>, FnoxError> {
        let fnox = self.build()?;
        fnox.list().map_err(map_fnox_err)
    }

    /// Build a `fnox::Fnox` from configured root + profile.
    #[cfg(feature = "fnox-library")]
    fn build(&self) -> Result<fnox::Fnox, FnoxError> {
        let mut f = if let Some(root) = &self.root {
            fnox::Fnox::open(root.join(fnox::library::CONFIG_FILENAME)).map_err(map_fnox_err)?
        } else {
            fnox::Fnox::discover().map_err(map_fnox_err)?
        };
        if let Some(p) = &self.profile {
            f = f.with_profile(p.clone());
        }
        Ok(f)
    }

    /// Stub when `fnox-library` feature is off — keeps the type
    /// constructable for downstream code that compile-checks both
    /// configurations.
    #[cfg(not(feature = "fnox-library"))]
    pub async fn get(&self, _name: &str) -> Result<String, FnoxError> {
        Err(FnoxError::FeatureDisabled {
            feature: "fnox-library",
        })
    }

    #[cfg(not(feature = "fnox-library"))]
    pub async fn list(&self) -> Result<Vec<String>, FnoxError> {
        Err(FnoxError::FeatureDisabled {
            feature: "fnox-library",
        })
    }

    // NOTE: `set` is intentionally NOT implemented. Upstream's
    // SetCommand::run is ~100 LOC of provider/encryption/remote-storage
    // orchestration and warrants its own design pass — a follow-up PR
    // to fnox will add `Fnox::set(name, value)` to the upstream
    // convenience API. Until then, callers needing `set` use the
    // subprocess `FnoxClient`.
}

#[cfg(feature = "fnox-library")]
fn map_fnox_err(e: fnox::FnoxError) -> FnoxError {
    // Coerce upstream's rich error enum to ours so callers don't need
    // to depend on `fnox::FnoxError` directly. exit_code is None
    // because no subprocess.
    FnoxError::Failed {
        exit_code: None,
        stderr: e.to_string(),
    }
}

#[cfg(all(test, not(feature = "fnox-library")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_feature_returns_accurate_error() {
        let client = FnoxLibrary::new();

        let get_err = client.get("OPENAI_API_KEY").await.unwrap_err();
        assert!(
            matches!(
                get_err,
                FnoxError::FeatureDisabled {
                    feature: "fnox-library"
                }
            ),
            "expected FeatureDisabled, got {get_err:?}"
        );

        let list_err = client.list().await.unwrap_err();
        assert!(
            matches!(
                list_err,
                FnoxError::FeatureDisabled {
                    feature: "fnox-library"
                }
            ),
            "expected FeatureDisabled, got {list_err:?}"
        );
    }
}
