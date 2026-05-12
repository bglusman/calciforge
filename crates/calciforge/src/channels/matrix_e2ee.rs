//! Experimental Matrix SDK E2EE scaffolding.
//!
//! This module is intentionally not used by the production Matrix channel yet.
//! It exists to keep the dependency/API probe close to the channel code and to
//! make the next refactor compile-gated instead of speculative.

use std::path::Path;

use matrix_sdk::Client;

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "experimental SDK scaffold is compile-gated until the Matrix loop is SDK-backed"
    )
)]
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

    #[tokio::test]
    async fn e2ee_builder_accepts_persistent_sqlite_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let client = e2ee_client_builder(
            "https://matrix.example.test",
            temp.path().join("matrix-sdk-store"),
            Some("test-passphrase"),
        )
        .build()
        .await
        .expect("SDK client should build with persistent E2EE store");

        let _encryption = client.encryption();
        assert_eq!(client.homeserver().as_str(), "https://matrix.example.test/");
    }
}
