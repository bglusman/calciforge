//! Persistent conversation context storage using SQLite.
//!
//! This module provides a SQLite-backed implementation of conversation context
//! storage that persists across restarts. It implements the same interface as
//! the in-memory `ContextStore` but stores exchanges in a SQLite database.

use anyhow::{Context as AnyhowContext, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::path::Path;

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
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
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

/// A watermark entry for an agent in a chat.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
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
    pool: SqlitePool,
    buffer_size: usize,
    inject_depth: usize,
}

impl PersistentContextStore {
    /// Create a new persistent context store.
    ///
    /// # Arguments
    /// * `database_url` - SQLite database URL (e.g., "sqlite:///path/to/context.db")
    /// * `buffer_size` - Maximum number of exchanges to retain per chat
    /// * `inject_depth` - Maximum number of unseen exchanges to inject
    pub async fn new(database_url: &str, buffer_size: usize, inject_depth: usize) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(
                database_url
                    .strip_prefix("sqlite://")
                    .unwrap_or(database_url),
            )
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        // Initialize database schema
        sqlx::query(SCHEMA).execute(&pool).await?;

        Ok(Self {
            pool,
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
        let path_str = path.as_ref().to_string_lossy();
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
        // Get the next sequence number for this chat
        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM exchanges WHERE chat_id = ?",
        )
        .bind(chat_id)
        .fetch_one(&self.pool)
        .await?;

        // Insert the new exchange
        sqlx::query(
            "INSERT INTO exchanges (chat_id, seq, sender_label, prompt, agent_id, response) 
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(chat_id)
        .bind(next_seq)
        .bind(sender_label)
        .bind(prompt)
        .bind(agent_id)
        .bind(response)
        .execute(&self.pool)
        .await?;

        // Update the agent's watermark
        sqlx::query(
            "INSERT OR REPLACE INTO watermarks (chat_id, agent_id, last_seen_seq) 
             VALUES (?, ?, ?)",
        )
        .bind(chat_id)
        .bind(agent_id)
        .bind(next_seq)
        .execute(&self.pool)
        .await?;

        // Enforce ring buffer capacity by deleting oldest exchanges
        if self.buffer_size > 0 {
            sqlx::query(
                "DELETE FROM exchanges 
                 WHERE chat_id = ? AND seq < (
                     SELECT seq FROM exchanges 
                     WHERE chat_id = ? 
                     ORDER BY seq DESC 
                     LIMIT 1 OFFSET ?
                 )",
            )
            .bind(chat_id)
            .bind(chat_id)
            .bind(self.buffer_size as i64 - 1)
            .execute(&self.pool)
            .await?;
        }

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

        // Get the agent's watermark (last seen sequence)
        let watermark: Option<i64> = sqlx::query_scalar(
            "SELECT last_seen_seq FROM watermarks WHERE chat_id = ? AND agent_id = ?",
        )
        .bind(chat_id)
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;

        // Query unseen exchanges (seq > watermark, or all if no watermark exists)
        let unseen_exchanges: Vec<PersistentExchange> = if let Some(wm) = watermark {
            sqlx::query_as(
                "SELECT * FROM exchanges 
                 WHERE chat_id = ? AND seq > ? 
                 ORDER BY seq ASC 
                 LIMIT ?",
            )
            .bind(chat_id)
            .bind(wm)
            .bind(self.inject_depth as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            // No watermark exists - agent hasn't seen any exchanges in this chat
            sqlx::query_as(
                "SELECT * FROM exchanges 
                 WHERE chat_id = ? 
                 ORDER BY seq DESC 
                 LIMIT ?",
            )
            .bind(chat_id)
            .bind(self.inject_depth as i64)
            .fetch_all(&self.pool)
            .await?
        };

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

    /// Clear all exchanges and watermarks for a chat.
    pub async fn clear(&self, chat_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM exchanges WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&self.pool)
            .await?;

        sqlx::query("DELETE FROM watermarks WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get the number of exchanges stored for a chat.
    pub async fn exchange_count(&self, chat_id: &str) -> Result<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM exchanges WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(count as usize)
    }

    /// Get all exchanges for a chat (for debugging/migration).
    pub async fn get_exchanges(&self, chat_id: &str) -> Result<Vec<PersistentExchange>> {
        let exchanges =
            sqlx::query_as("SELECT * FROM exchanges WHERE chat_id = ? ORDER BY seq ASC")
                .bind(chat_id)
                .fetch_all(&self.pool)
                .await?;

        Ok(exchanges)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    async fn create_test_store() -> Result<PersistentContextStore> {
        let temp_file = NamedTempFile::new()?;
        let store = PersistentContextStore::with_file_path(
            temp_file.path(),
            20, // buffer_size
            5,  // inject_depth
        )
        .await?;
        Ok(store)
    }

    #[tokio::test]
    async fn test_push_and_retrieve() -> Result<()> {
        let store = create_test_store().await?;

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
        let store = create_test_store().await?;

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
        let store = create_test_store().await?;

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
        let temp_file = NamedTempFile::new()?;
        let store = PersistentContextStore::with_file_path(
            temp_file.path(),
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
        let store = create_test_store().await?;

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
        let temp_file = NamedTempFile::new()?;
        let store = PersistentContextStore::with_file_path(
            temp_file.path(),
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
