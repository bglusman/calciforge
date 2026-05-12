//! Experimental Matrix SDK E2EE scaffolding.
//!
//! This module is intentionally not used by the production Matrix channel yet.
//! It exists to keep the dependency/API probe close to the channel code and to
//! make the next refactor compile-gated instead of speculative.

use std::path::Path;

use matrix_sdk::Client;

// Kept compile-gated until the Matrix channel loop is SDK-backed.
#[allow(dead_code)]
pub fn e2ee_client_builder(
    homeserver: &str,
    store_path: impl AsRef<Path>,
    store_passphrase: Option<&str>,
) -> matrix_sdk::ClientBuilder {
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(store_path, store_passphrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2ee_builder_accepts_persistent_sqlite_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let builder = e2ee_client_builder(
            "https://matrix.example.test",
            temp.path().join("matrix-sdk-store"),
            Some("test-passphrase"),
        );

        let debug = format!("{builder:?}");
        assert!(
            debug.contains("matrix.example.test"),
            "builder should retain configured homeserver in debug output: {debug}"
        );
    }
}
