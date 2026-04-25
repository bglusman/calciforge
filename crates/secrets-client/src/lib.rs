//! Secrets client for Calciforge.
//!
//! Provides:
//! - [`vault::get_secret`] — env → fnox → vaultwarden resolver chain
//!   (read-only)
//! - [`FnoxClient`] — subprocess wrapper around the `fnox` CLI
//!   (read + write)
//! - [`FnoxError`] — typed errors from `FnoxClient`
//! - [`config::RetryConfig`] — retry-policy struct used by the
//!   calciforge proxy
//!
//! Renamed from `onecli-client` after the migration away from the
//! OneCLI binary as a credential proxy. The HTTP client + binary
//! that lived here previously have been deleted (zero external
//! callers); this crate is now a focused secrets-resolution library.

pub mod config;
pub mod fnox_client;
pub mod vault;

pub use config::RetryConfig;
pub use fnox_client::{FnoxClient, FnoxError};
