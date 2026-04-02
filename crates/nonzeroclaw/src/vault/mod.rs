//! Vault integration — credential management with per-secret approval policies.
//!
//! This module provides a backend-agnostic interface for storing and retrieving
//! secrets, gated behind configurable approval policies. The design follows the
//! plan in `docs/vault-integration-plan.md` and `research/approval-relay-design.md`.
//!
//! # Architecture
//!
//! ```text
//! VaultManager
//!   ├── VaultAdapter  (trait) — get/store/unlock secrets
//!   │     ├── BitwardenCliAdapter  (feature = "bitwarden-cli") — `bw` subprocess
//!   │     └── NoopVaultAdapter    — always returns NotConfigured error
//!   └── ApprovalRelay (trait) — channel-agnostic human-in-the-loop
//!         ├── NoopApprovalRelay   — always approves (policy = Auto)
//!         └── ChannelApprovalRelay — sends to channel, awaits response
//! ```
//!
//! # Usage
//!
//! ```toml
//! [vault]
//! backend = "bitwarden-cli"
//! bw_path = "bw"
//!
//! [vault.secrets.anthropic_key]
//! bw_item_id = "anthropic-api-key"
//! policy = "auto"
//!
//! [vault.secrets.stripe_key]
//! bw_item_id = "stripe-live-key"
//! policy = "per-use"
//! ```

pub mod adapter;
pub mod approval;
pub mod config;
pub mod error;
pub mod manager;
pub mod types;

#[cfg(feature = "bitwarden-cli")]
pub mod bitwarden;

// Re-exports for ergonomic use from outside this module
pub use adapter::VaultAdapter;
pub use approval::{ApprovalDecision, ApprovalRelay, ChannelApprovalRelay, NoopApprovalRelay};
pub use config::{SecretPolicyConfig, VaultBackend, VaultConfig, VaultSecretConfig};
pub use error::VaultError;
pub use manager::VaultManager;
pub use types::{Secret, SecretPolicy, SecretValue, SessionToken};

#[cfg(feature = "bitwarden-cli")]
pub use bitwarden::BitwardenCliAdapter;
