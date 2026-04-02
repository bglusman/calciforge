//! Config schema for the `[vault]` section of `config.toml`.
//!
//! These types are added to the top-level `Config` struct in
//! `crates/nonzeroclaw/src/config/schema.rs`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── VaultBackend ─────────────────────────────────────────────────────────────

/// Which vault backend to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum VaultBackend {
    /// Disabled — vault calls return `VaultError::NotConfigured`.
    None,
    /// Use the `bw` CLI subprocess to talk to Bitwarden / Vaultwarden.
    #[serde(rename = "bitwarden-cli")]
    BitwardenCli,
}

impl Default for VaultBackend {
    fn default() -> Self {
        Self::None
    }
}

// ── SecretPolicyConfig ───────────────────────────────────────────────────────

/// The per-secret access policy serialized in `config.toml`.
///
/// Maps to the runtime `SecretPolicy` enum after parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SecretPolicyConfig {
    /// Inject silently — no approval required.
    #[default]
    Auto,
    /// Require approval on every access.
    PerUse,
    /// Approve once per agent session.
    Session,
    /// Approve once; valid for `ttl_secs` seconds.
    TimeBound,
}

// ── VaultSecretConfig ────────────────────────────────────────────────────────

/// Per-secret configuration block.
///
/// ```toml
/// [vault.secrets.anthropic_key]
/// bw_item_id = "anthropic-api-key"
/// policy = "auto"
///
/// [vault.secrets.stripe_key]
/// bw_item_id = "stripe-live-key"
/// policy = "per-use"
///
/// [vault.secrets.deploy_key]
/// bw_item_id = "deploy-ssh-key"
/// policy = "time-bound"
/// ttl_secs = 14400   # 4 hours
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VaultSecretConfig {
    /// Bitwarden item ID or name.
    ///
    /// Passed directly to `bw get password <bw_item_id>`.
    pub bw_item_id: String,

    /// Access policy for this secret.
    #[serde(default)]
    pub policy: SecretPolicyConfig,

    /// TTL in seconds — only used when `policy = "time-bound"`.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

// ── VaultConfig ──────────────────────────────────────────────────────────────

/// Top-level vault configuration (`[vault]` section).
///
/// ```toml
/// [vault]
/// backend = "bitwarden-cli"
/// bw_path = "bw"        # defaults to "bw" on PATH
///
/// [vault.secrets.my_key]
/// bw_item_id = "uuid-or-name"
/// policy = "auto"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VaultConfig {
    /// Which backend to use.  Default: `none` (disabled).
    #[serde(default)]
    pub backend: VaultBackend,

    /// Path to the `bw` binary.  Default: `"bw"` (resolved from `PATH`).
    #[serde(default = "default_bw_path")]
    pub bw_path: String,

    /// Timeout for the vault session token in seconds.
    ///
    /// After this period the adapter will automatically re-unlock.
    /// Default: 3600 (1 hour).
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,

    /// Per-secret configuration entries.
    #[serde(default)]
    pub secrets: HashMap<String, VaultSecretConfig>,
}

fn default_bw_path() -> String {
    "bw".to_string()
}

fn default_session_ttl_secs() -> u64 {
    3600
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            backend: VaultBackend::None,
            bw_path: default_bw_path(),
            session_ttl_secs: default_session_ttl_secs(),
            secrets: HashMap::new(),
        }
    }
}

// ── Conversion helpers ───────────────────────────────────────────────────────

impl VaultSecretConfig {
    /// Convert to the runtime `SecretPolicy`.
    pub fn to_runtime_policy(&self) -> crate::vault::SecretPolicy {
        use crate::vault::SecretPolicy;
        use std::time::Duration;

        match self.policy {
            SecretPolicyConfig::Auto => SecretPolicy::Auto,
            SecretPolicyConfig::PerUse => SecretPolicy::PerUse,
            SecretPolicyConfig::Session => SecretPolicy::Session,
            SecretPolicyConfig::TimeBound => {
                let ttl_secs = self.ttl_secs.unwrap_or(3600);
                SecretPolicy::TimeBound {
                    ttl: Duration::from_secs(ttl_secs),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_config_default_is_disabled() {
        let cfg = VaultConfig::default();
        assert_eq!(cfg.backend, VaultBackend::None);
        assert_eq!(cfg.bw_path, "bw");
        assert_eq!(cfg.session_ttl_secs, 3600);
        assert!(cfg.secrets.is_empty());
    }

    #[test]
    fn vault_config_serde_roundtrip() {
        let toml_str = r#"
            backend = "bitwarden-cli"
            bw_path = "/usr/local/bin/bw"
            session_ttl_secs = 7200

            [secrets.anthropic_key]
            bw_item_id = "anthropic-api-key"
            policy = "auto"

            [secrets.stripe_key]
            bw_item_id = "stripe-live-key"
            policy = "per-use"

            [secrets.deploy_key]
            bw_item_id = "deploy-ssh-key"
            policy = "time-bound"
            ttl_secs = 14400
        "#;

        let cfg: VaultConfig = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.backend, VaultBackend::BitwardenCli);
        assert_eq!(cfg.bw_path, "/usr/local/bin/bw");
        assert_eq!(cfg.session_ttl_secs, 7200);
        assert_eq!(cfg.secrets.len(), 3);

        let deploy = cfg.secrets.get("deploy_key").expect("deploy_key present");
        assert_eq!(deploy.policy, SecretPolicyConfig::TimeBound);
        assert_eq!(deploy.ttl_secs, Some(14400));
    }

    #[test]
    fn secret_policy_config_converts_to_runtime() {
        use crate::vault::SecretPolicy;
        use std::time::Duration;

        let auto_cfg = VaultSecretConfig {
            bw_item_id: "x".to_string(),
            policy: SecretPolicyConfig::Auto,
            ttl_secs: None,
        };
        assert_eq!(auto_cfg.to_runtime_policy(), SecretPolicy::Auto);

        let tb_cfg = VaultSecretConfig {
            bw_item_id: "x".to_string(),
            policy: SecretPolicyConfig::TimeBound,
            ttl_secs: Some(7200),
        };
        assert_eq!(
            tb_cfg.to_runtime_policy(),
            SecretPolicy::TimeBound {
                ttl: Duration::from_secs(7200)
            }
        );
    }
}
