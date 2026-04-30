//! Persistent conversation context storage using SQLite.
//!
//! This module provides a SQLite-backed implementation of conversation context
//! storage that persists across restarts. It implements the same interface as
//! the in-memory `ContextStore` but stores exchanges in a SQLite database.
//!
//! Backed by `rusqlite` (synchronous C API). The connection lives behind a
//! `std::sync::Mutex` and all DB calls run inside `tokio::task::spawn_blocking`
//! so the async caller stays unblocked. The mutex is only held within the
//! blocking closures, never across an `.await`.

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Database schema
// ---------------------------------------------------------------------------

/// SQL schema for the conversation context database.
pub const SCHEMA: &str = r#"
-- Conversation exchanges table
CREATE TABLE IF NOT EXISTS exchanges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    sender_label TEXT NOT NULL,
    prompt TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    response TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(chat_id, seq)
);

-- Agent watermarks table (last seen exchange per agent per chat)
CREATE TABLE IF NOT EXISTS watermarks (
    chat_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    last_seen_seq INTEGER NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (chat_id, agent_id)
);

-- Indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_exchanges_chat_seq ON exchanges(chat_id, seq);
CREATE INDEX IF NOT EXISTS idx_exchanges_created_at ON exchanges(created_at);
"#;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single exchange stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentExchange {
    pub id: i64,
    pub chat_id: String,
    pub seq: i64,
    pub sender_label: String,
    pub prompt: String,
    pub agent_id: String,
    pub response: String,
    pub created_at: DateTime<Utc>,
}

impl PersistentExchange {
    fn try_from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        // SQLite's CURRENT_TIMESTAMP yields "YYYY-MM-DD HH:MM:SS" (UTC, no tz).
        let created_at_str: String = row.get("created_at")?;
        let created_at = parse_sqlite_timestamp(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;

        Ok(Self {
            id: row.get("id")?,
            chat_id: row.get("chat_id")?,
            seq: row.get("seq")?,
            sender_label: row.get("sender_label")?,
            prompt: row.get("prompt")?,
            agent_id: row.get("agent_id")?,
            response: row.get("response")?,
            created_at,
        })
    }
}

fn parse_sqlite_timestamp(s: &str) -> Result<DateTime<Utc>, String> {
    // Try the canonical SQLite default format first.
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    // RFC3339 fallback in case someone has stored ISO8601 strings.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    Err(format!("unrecognized timestamp format: {s}"))
}

/// A watermark entry for an agent in a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct Watermark {
    pub chat_id: String,
    pub agent_id: String,
    pub last_seen_seq: i64,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// PersistentContextStore
// ---------------------------------------------------------------------------

/// SQLite-backed persistent conversation context store.
///
/// This store maintains the same semantics as the in-memory `ContextStore`:
/// - Ring buffer behavior (evicts oldest exchanges when capacity is reached)
/// - Per-agent watermarks to track what each agent has seen
/// - Context injection for agents that haven't seen recent exchanges
///
/// The main difference is that exchanges are persisted to a SQLite database
/// and survive process restarts.
pub struct PersistentContextStore {
    conn: Arc<Mutex<Connection>>,
    buffer_size: usize,
    inject_depth: usize,
}

#[allow(dead_code)]
impl PersistentContextStore {
    /// Create a new persistent context store.
    ///
    /// # Arguments
    /// * `database_url` - SQLite database URL or path (e.g.,
    ///   `"sqlite:///path/to/context.db"` or `"/path/to/context.db"`)
    /// * `buffer_size` - Maximum number of exchanges to retain per chat
    /// * `inject_depth` - Maximum number of unseen exchanges to inject
    pub async fn new(database_url: &str, buffer_size: usize, inject_depth: usize) -> Result<Self> {
        let path = database_url
            .strip_prefix("sqlite://")
            .unwrap_or(database_url)
            .to_string();

        let conn = tokio::task::spawn_blocking(move || -> rusqlite::Result<Connection> {
            let conn = Connection::open(&path)?;
            conn.execute_batch(SCHEMA)?;
            Ok(conn)
        })
        .await??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            buffer_size,
            inject_depth,
        })
    }

    /// Create a persistent context store with a file path.
    ///
    /// This is a convenience method that accepts a file path instead of a full URL.
    pub async fn with_file_path<P: AsRef<Path>>(
        path: P,
        buffer_size: usize,
        inject_depth: usize,
    ) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().into_owned();
        Self::new(&format!("sqlite://{}", path_str), buffer_size, inject_depth).await
    }

    /// Push a new exchange into the store and advance the agent's watermark.
    pub async fn push(
        &self,
        chat_id: &str,
        sender_label: &str,
        prompt: &str,
        agent_id: &str,
        response: &str,
    ) -> Result<()> {
        let conn = Arc::clone(&self.conn);
        let buffer_size = self.buffer_size;
        let chat_id = chat_id.to_string();
        let sender_label = sender_label.to_string();
        let prompt = prompt.to_string();
        let agent_id = agent_id.to_string();
        let response = response.to_string();

        tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
            let conn = conn.lock().unwrap();

            let next_seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM exchanges WHERE chat_id = ?",
                params![chat_id],
                |row| row.get(0),
            )?;

            conn.execute(
                "INSERT INTO exchanges (chat_id, seq, sender_label, prompt, agent_id, response) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![chat_id, next_seq, sender_label, prompt, agent_id, response],
            )?;

            conn.execute(
                "INSERT OR REPLACE INTO watermarks (chat_id, agent_id, last_seen_seq) \
                 VALUES (?, ?, ?)",
                params![chat_id, agent_id, next_seq],
            )?;

            if buffer_size > 0 {
                conn.execute(
                    "DELETE FROM exchanges \
                     WHERE chat_id = ? AND seq < ( \
                         SELECT seq FROM exchanges \
                         WHERE chat_id = ? \
                         ORDER BY seq DESC \
                         LIMIT 1 OFFSET ? \
                     )",
                    params![chat_id, chat_id, buffer_size as i64 - 1],
                )?;
            }

            Ok(())
        })
        .await??;

        Ok(())
    }

    /// Build a context preamble for an agent, injecting up to `inject_depth`
    /// exchanges that the agent has NOT yet seen.
    ///
    /// Returns `None` if there are no unseen exchanges or if `inject_depth` is 0.
    pub async fn build_preamble(&self, chat_id: &str, agent_id: &str) -> Result<Option<String>> {
        if self.inject_depth == 0 {
            return Ok(None);
        }

        let conn = Arc::clone(&self.conn);
        let inject_depth = self.inject_depth;
        let chat_id = chat_id.to_string();
        let agent_id = agent_id.to_string();

        let unseen_exchanges = tokio::task::spawn_blocking(
            move || -> rusqlite::Result<Vec<PersistentExchange>> {
                let conn = conn.lock().unwrap();

                let watermark: Option<i64> = conn
                    .query_row(
                        "SELECT last_seen_seq FROM watermarks WHERE chat_id = ? AND agent_id = ?",
                        params![chat_id, agent_id],
                        |row| row.get(0),
                    )
                    .optional()?;

                if let Some(wm) = watermark {
                    let mut stmt = conn.prepare(
                        "SELECT id, chat_id, seq, sender_label, prompt, agent_id, response, created_at \
                         FROM exchanges \
                         WHERE chat_id = ? AND seq > ? \
                         ORDER BY seq ASC \
                         LIMIT ?",
                    )?;
                    let rows = stmt.query_map(
                        params![chat_id, wm, inject_depth as i64],
                        PersistentExchange::try_from_row,
                    )?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()
                } else {
                    // No watermark exists - agent hasn't seen any exchanges in this chat.
                    // Take the most recent N (ordered DESC), then re-sort ASC for display.
                    let mut stmt = conn.prepare(
                        "SELECT id, chat_id, seq, sender_label, prompt, agent_id, response, created_at \
                         FROM exchanges \
                         WHERE chat_id = ? \
                         ORDER BY seq DESC \
                         LIMIT ?",
                    )?;
                    let rows = stmt.query_map(
                        params![chat_id, inject_depth as i64],
                        PersistentExchange::try_from_row,
                    )?;
                    let mut v = rows.collect::<rusqlite::Result<Vec<_>>>()?;
                    v.sort_by_key(|e| e.seq);
                    Ok(v)
                }
            },
        )
        .await??;

        if unseen_exchanges.is_empty() {
            return Ok(None);
        }

        // Format the preamble
        let mut lines = vec!["[Recent context:".to_string()];
        for ex in unseen_exchanges {
            lines.push(format!("{}: {}", ex.sender_label, ex.prompt));
            lines.push(format!("{}: {}", ex.agent_id, ex.response));
        }
        lines.push("]".to_string());

        Ok(Some(lines.join("\n")))
    }

    /// Build a context preamble and prepend it to `message`. If there is no
    /// preamble (agent has seen everything, or `inject_depth = 0`), the
    /// original message is returned unchanged.
    pub async fn augment_message(
        &self,
        chat_id: &str,
        agent_id: &str,
        message: &str,
    ) -> Result<String> {
        match self.build_preamble(chat_id, agent_id).await? {
            Some(preamble) => Ok(format!("{preamble}\n{message}")),
            None => Ok(message.to_string()),
        }
    }

    /// Clear all exchanges and watermarks for a chat.
    pub async fn clear(&self, chat_id: &str) -> Result<()> {
        let conn = Arc::clone(&self.conn);
        let chat_id = chat_id.to_string();

        tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
            let conn = conn.lock().unwrap();
            conn.execute("DELETE FROM exchanges WHERE chat_id = ?", params![chat_id])?;
            conn.execute("DELETE FROM watermarks WHERE chat_id = ?", params![chat_id])?;
            Ok(())
        })
        .await??;

        Ok(())
    }

    /// Get the number of exchanges stored for a chat.
    pub async fn exchange_count(&self, chat_id: &str) -> Result<usize> {
        let conn = Arc::clone(&self.conn);
        let chat_id = chat_id.to_string();

        let count = tokio::task::spawn_blocking(move || -> rusqlite::Result<i64> {
            let conn = conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM exchanges WHERE chat_id = ?",
                params![chat_id],
                |row| row.get(0),
            )
        })
        .await??;

        Ok(count as usize)
    }

    /// Get all exchanges for a chat (for debugging/migration).
    pub async fn get_exchanges(&self, chat_id: &str) -> Result<Vec<PersistentExchange>> {
        let conn = Arc::clone(&self.conn);
        let chat_id = chat_id.to_string();

        let exchanges =
            tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<PersistentExchange>> {
                let conn = conn.lock().unwrap();
                let mut stmt = conn.prepare(
                    "SELECT id, chat_id, seq, sender_label, prompt, agent_id, response, created_at \
                     FROM exchanges WHERE chat_id = ? ORDER BY seq ASC",
                )?;
                let rows = stmt.query_map(params![chat_id], PersistentExchange::try_from_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await??;

        Ok(exchanges)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn fresh_db_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("ctx.db")
    }

    async fn create_test_store() -> Result<(TempDir, PersistentContextStore)> {
        let dir = tempdir()?;
        let store = PersistentContextStore::with_file_path(
            fresh_db_path(&dir),
            20, // buffer_size
            5,  // inject_depth
        )
        .await?;
        Ok((dir, store))
    }

    #[tokio::test]
    async fn test_push_and_retrieve() -> Result<()> {
        let (_dir, store) = create_test_store().await?;

        // Push an exchange
        store
            .push("chat:1", "Brian", "Hello", "librarian", "Hi there!")
            .await?;

        // Verify exchange count
        let count = store.exchange_count("chat:1").await?;
        assert_eq!(count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_preamble_for_new_agent() -> Result<()> {
        let (_dir, store) = create_test_store().await?;

        // Push some exchanges
        store
            .push("chat:1", "Brian", "Q1", "librarian", "A1")
            .await?;
        store
            .push("chat:1", "Brian", "Q2", "librarian", "A2")
            .await?;

        // New agent should see all exchanges
        let preamble = store.build_preamble("chat:1", "custodian").await?;
        assert!(preamble.is_some());
        let preamble_text = preamble.unwrap();
        assert!(preamble_text.contains("Q1"));
        assert!(preamble_text.contains("Q2"));

        Ok(())
    }

    #[tokio::test]
    async fn test_preamble_for_existing_agent() -> Result<()> {
        let (_dir, store) = create_test_store().await?;

        // Agent answers first exchange
        store
            .push("chat:1", "Brian", "Q1", "librarian", "A1")
            .await?;

        // Same agent should not see its own exchange
        let preamble = store.build_preamble("chat:1", "librarian").await?;
        assert!(preamble.is_none());

        // Different agent answers second exchange
        store
            .push("chat:1", "Brian", "Q2", "custodian", "A2")
            .await?;

        // First agent should see second exchange
        let preamble = store.build_preamble("chat:1", "librarian").await?;
        assert!(preamble.is_some());
        let preamble_text = preamble.unwrap();
        assert!(preamble_text.contains("Q2"));
        assert!(!preamble_text.contains("Q1"));

        Ok(())
    }

    #[tokio::test]
    async fn test_ring_buffer_capacity() -> Result<()> {
        // Create store with small buffer
        let dir = tempdir()?;
        let store = PersistentContextStore::with_file_path(
            fresh_db_path(&dir),
            3, // buffer_size = 3
            5, // inject_depth
        )
        .await?;

        // Push more exchanges than buffer size
        for i in 0..5 {
            store
                .push(
                    "chat:1",
                    "Brian",
                    &format!("Q{}", i),
                    "librarian",
                    &format!("A{}", i),
                )
                .await?;
        }

        // Should only have buffer_size exchanges
        let count = store.exchange_count("chat:1").await?;
        assert_eq!(count, 3);

        // Should have the most recent exchanges
        let exchanges = store.get_exchanges("chat:1").await?;
        assert_eq!(exchanges.len(), 3);
        assert_eq!(exchanges[0].prompt, "Q2"); // Oldest in buffer
        assert_eq!(exchanges[2].prompt, "Q4"); // Newest in buffer

        Ok(())
    }

    #[tokio::test]
    async fn test_clear_chat() -> Result<()> {
        let (_dir, store) = create_test_store().await?;

        store
            .push("chat:1", "Brian", "Q1", "librarian", "A1")
            .await?;
        store
            .push("chat:2", "Brian", "Q2", "librarian", "A2")
            .await?;

        assert_eq!(store.exchange_count("chat:1").await?, 1);
        assert_eq!(store.exchange_count("chat:2").await?, 1);

        store.clear("chat:1").await?;

        assert_eq!(store.exchange_count("chat:1").await?, 0);
        assert_eq!(store.exchange_count("chat:2").await?, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_inject_depth_limit() -> Result<()> {
        // Create store with inject_depth = 2
        let dir = tempdir()?;
        let store = PersistentContextStore::with_file_path(
            fresh_db_path(&dir),
            20, // buffer_size
            2,  // inject_depth = 2
        )
        .await?;

        // Push 5 exchanges
        for i in 0..5 {
            store
                .push(
                    "chat:1",
                    "Brian",
                    &format!("Q{}", i),
                    "librarian",
                    &format!("A{}", i),
                )
                .await?;
        }

        // New agent should only see last 2 exchanges (due to inject_depth)
        let preamble = store.build_preamble("chat:1", "custodian").await?;
        assert!(preamble.is_some());
        let preamble_text = preamble.unwrap();

        // Should contain Q3 and Q4 (most recent 2)
        assert!(preamble_text.contains("Q3"));
        assert!(preamble_text.contains("Q4"));
        // Should NOT contain Q2 (older than depth limit)
        assert!(!preamble_text.contains("Q2"));

        Ok(())
    }
}
