//! Content digest store — persists URL → (SHA-256 hash, verdict, timestamp) across runs.
//!
//! When a URL is fetched, the content is hashed. If the same URL is seen again and
//! the digest matches, the previously computed verdict is reused without re-scanning.
//! If the digest has changed the content is rescanned automatically.
//!
//! Persistence is via a simple JSON file; the default path is `~/.outpost/digests.json`.
//! The file is loaded eagerly on construction and flushed after every mutation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf};
use tokio::{fs, io::AsyncWriteExt};
use tracing::warn;

use crate::verdict::OutpostVerdict;

// ── ContentDigest ────────────────────────────────────────────────────────────

/// A persisted record for one URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDigest {
    /// SHA-256 hex digest of the content bytes.
    pub sha256: String,
    /// The cached verdict from the last scan of this digest.
    pub verdict: OutpostVerdict,
    /// UTC timestamp of when this entry was last written.
    pub timestamp: DateTime<Utc>,
    /// If `true`, a human explicitly approved this URL+digest.
    /// Override entries bypass future `Unsafe` or `Review` verdicts.
    pub override_approved: bool,
}

// ── DigestStore ──────────────────────────────────────────────────────────────

/// Persistent URL → [`ContentDigest`] store backed by a JSON file.
///
/// All mutating methods flush the store to disk immediately so entries survive
/// process restarts.
pub struct DigestStore {
    path: PathBuf,
    entries: HashMap<String, ContentDigest>,
}

impl DigestStore {
    /// Open (or create) the store at `path`.
    ///
    /// If the file does not exist it is created on the first flush. If it exists
    /// but cannot be parsed, a warning is logged and the store starts empty.
    pub async fn open(path: PathBuf) -> Self {
        let entries = Self::load(&path).await;
        Self { path, entries }
    }

    /// Open the store at the default path: `~/.outpost/digests.json`.
    pub async fn open_default() -> Self {
        let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
        Self::open(home.join(".outpost/digests.json")).await
    }

    /// Look up a URL by exact match. Returns `None` if not found.
    pub fn get(&self, url: &str) -> Option<&ContentDigest> {
        self.entries.get(url)
    }

    /// Insert or replace the entry for `url` and flush to disk.
    pub async fn set(&mut self, url: &str, entry: ContentDigest) {
        self.entries.insert(url.to_owned(), entry);
        self.flush().await;
    }

    /// Mark a URL+digest as human-approved, bypassing future blocks for that
    /// exact content hash. Flushes to disk.
    ///
    /// If the URL is not yet in the store this is a no-op (the override is only
    /// meaningful for a known digest).
    pub async fn mark_override(&mut self, url: &str, digest: &str) {
        if let Some(entry) = self.entries.get_mut(url) {
            if entry.sha256 == digest {
                entry.override_approved = true;
                self.flush().await;
            }
        }
    }

    // ── private helpers ──────────────────────────────────────────────────────

    async fn load(path: &PathBuf) -> HashMap<String, ContentDigest> {
        match fs::read_to_string(path).await {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                warn!("digest store: failed to parse {}: {e}", path.display());
                HashMap::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                warn!("digest store: failed to read {}: {e}", path.display());
                HashMap::new()
            }
        }
    }

    async fn flush(&self) {
        let data = match serde_json::to_string_pretty(&self.entries) {
            Ok(d) => d,
            Err(e) => {
                warn!("digest store: serialize error: {e}");
                return;
            }
        };
        if let Some(parent) = self.path.parent() {
            if let Err(e) = fs::create_dir_all(parent).await {
                warn!("digest store: mkdir error: {e}");
                return;
            }
        }
        match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)
            .await
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data.as_bytes()).await {
                    warn!("digest store: write error: {e}");
                }
            }
            Err(e) => warn!("digest store: open error: {e}"),
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of `content`.
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn tmp_path() -> PathBuf {
        // We only need the path; we drop the file handle so the store can reopen it.
        let f = NamedTempFile::new().expect("tempfile");
        let p = f.path().to_path_buf();
        // Remove so the store starts with an empty file
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn test_empty_store_returns_none() {
        let store = DigestStore::open(tmp_path()).await;
        assert!(store.get("https://example.com").is_none());
    }

    #[tokio::test]
    async fn test_set_and_get_roundtrip() {
        let path = tmp_path();
        let mut store = DigestStore::open(path.clone()).await;
        let digest = sha256_hex("hello world");
        store
            .set(
                "https://example.com",
                ContentDigest {
                    sha256: digest.clone(),
                    verdict: OutpostVerdict::Clean,
                    timestamp: Utc::now(),
                    override_approved: false,
                },
            )
            .await;

        // Reload from disk to verify persistence
        let store2 = DigestStore::open(path).await;
        let entry = store2.get("https://example.com").expect("entry should persist");
        assert_eq!(entry.sha256, digest);
        assert!(entry.verdict.is_clean());
    }

    #[tokio::test]
    async fn test_mark_override_sets_flag() {
        let path = tmp_path();
        let mut store = DigestStore::open(path.clone()).await;
        let digest = sha256_hex("some content");
        store
            .set(
                "https://example.com/page",
                ContentDigest {
                    sha256: digest.clone(),
                    verdict: OutpostVerdict::Unsafe { reason: "test".into() },
                    timestamp: Utc::now(),
                    override_approved: false,
                },
            )
            .await;

        assert!(!store.get("https://example.com/page").unwrap().override_approved);
        store.mark_override("https://example.com/page", &digest).await;
        assert!(store.get("https://example.com/page").unwrap().override_approved);
    }

    #[tokio::test]
    async fn test_mark_override_wrong_digest_noop() {
        let path = tmp_path();
        let mut store = DigestStore::open(path).await;
        let digest = sha256_hex("content a");
        store
            .set(
                "https://example.com",
                ContentDigest {
                    sha256: digest,
                    verdict: OutpostVerdict::Unsafe { reason: "test".into() },
                    timestamp: Utc::now(),
                    override_approved: false,
                },
            )
            .await;

        // Wrong digest — should not flip flag
        store.mark_override("https://example.com", &sha256_hex("content b")).await;
        assert!(!store.get("https://example.com").unwrap().override_approved);
    }

    #[test]
    fn test_sha256_hex_deterministic() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert_ne!(a, sha256_hex("world"));
        // Known SHA-256 of "hello"
        assert_eq!(a, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }
}
