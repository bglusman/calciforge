//! Secrets client for Calciforge.
//!
//! Provides:
//! - [`vault::get_secret`] — env → fnox → vaultwarden resolver chain
//!   (read-only)
//! - [`FnoxClient`] — subprocess wrapper around the `fnox` CLI
//!   (read + write)
//! - [`FnoxLibrary`] — optional direct `fnox` library wrapper
//!   (read + list, enabled with the `fnox-library` feature)
//! - [`FnoxError`] — typed errors from `FnoxClient`
//! - [`config::RetryConfig`] — retry-policy struct used by the
//!   calciforge proxy
//! - [`secret_reference_token`] — helper for stable `{secret:NAME}`
//!   references used by agent-facing tooling
//!
//! Renamed from `onecli-client` after the migration away from the
//! OneCLI binary as a credential proxy. The HTTP client + binary
//! that lived here previously have been deleted (zero external
//! callers); this crate is now a focused secrets-resolution library.

pub mod config;
pub mod fnox_client;
pub mod fnox_library;
pub mod secret_refs;
pub mod vault;

pub use config::RetryConfig;
pub use fnox_client::{FnoxClient, FnoxError};
pub use fnox_library::FnoxLibrary;
pub use secret_refs::{is_valid_secret_name, secret_reference_token};
