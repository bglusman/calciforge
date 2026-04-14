//! Unified conversation context store supporting both in-memory and persistent storage.
//!
//! This module provides a unified interface for conversation context storage
//! that can use either in-memory storage (`ContextStore`) or persistent SQLite
//! storage (`PersistentContextStore`) based on configuration.

use crate::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;

// Re-export the existing in-memory store
pub use crate::context::ContextStore as InMemoryContextStore;

#[cfg(feature = "persistent-context")]
use crate::persistent_context::PersistentContextStore;

// ---------------------------------------------------------------------------
// ContextStore trait
// ---------------------------------------------------------------------------

/// Unified trait for conversation context storage.
#[async_trait]
#[allow(dead_code)]
pub trait ContextStoreTrait: Send + Sync {
    /// Build a context preamble for a chat+agent pair and return the full
    /// message to send (preamble prepended if non-empty).
    async fn augment_message(&self, chat_id: &str, agent_id: &str, message: &str)
        -> Result<String>;

    /// Record a completed exchange and advance the agent's watermark.
    async fn push(
        &self,
        chat_id: &str,
        sender_label: &str,
        prompt: &str,
        agent_id: &str,
        response: &str,
    ) -> Result<()>;

    /// Clear the conversation context for a chat.
    async fn clear(&self, chat_id: &str) -> Result<()>;

    /// Return the number of exchanges stored for a chat.
    async fn exchange_count(&self, chat_id: &str) -> Result<usize>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ContextStoreTrait for InMemoryContextStore {
    async fn augment_message(
        &self,
        chat_id: &str,
        agent_id: &str,
        message: &str,
    ) -> Result<String> {
        // The in-memory store's augment_message is synchronous
        Ok(self.augment_message(chat_id, agent_id, message))
    }

    async fn push(
        &self,
        chat_id: &str,
        sender_label: &str,
        prompt: &str,
        agent_id: &str,
        response: &str,
    ) -> Result<()> {
        // The in-memory store's push is synchronous
        self.push(chat_id, sender_label, prompt, agent_id, response);
        Ok(())
    }

    async fn clear(&self, chat_id: &str) -> Result<()> {
        // The in-memory store's clear is synchronous
        self.clear(chat_id);
        Ok(())
    }

    async fn exchange_count(&self, chat_id: &str) -> Result<usize> {
        // The in-memory store's exchange_count is synchronous
        Ok(self.exchange_count(chat_id))
    }
}

// ---------------------------------------------------------------------------
// Unified ContextStore enum
// ---------------------------------------------------------------------------

/// Unified context store that can be either in-memory or persistent.
#[derive(Clone)]
pub enum UnifiedContextStore {
    InMemory(Arc<InMemoryContextStore>),
    #[cfg(feature = "persistent-context")]
    Persistent(Arc<PersistentContextStore>),
}

impl UnifiedContextStore {
    /// Create a new context store based on configuration.
    pub async fn new(
        buffer_size: usize,
        inject_depth: usize,
        persistent_config: Option<&crate::config::PersistentContextConfig>,
    ) -> Result<Self> {
        match persistent_config {
            #[cfg(feature = "persistent-context")]
            Some(config) => {
                // Create persistent store
                let store =
                    PersistentContextStore::new(&config.database_url, buffer_size, inject_depth)
                        .await?;
                Ok(Self::Persistent(Arc::new(store)))
            }
            _ => {
                // Create in-memory store
                let store = InMemoryContextStore::new(buffer_size, inject_depth);
                Ok(Self::InMemory(Arc::new(store)))
            }
        }
    }
}

#[async_trait]
impl ContextStoreTrait for UnifiedContextStore {
    async fn augment_message(
        &self,
        chat_id: &str,
        agent_id: &str,
        message: &str,
    ) -> Result<String> {
        match self {
            Self::InMemory(store) => Ok(store.augment_message(chat_id, agent_id, message)),
            #[cfg(feature = "persistent-context")]
            Self::Persistent(store) => store.augment_message(chat_id, agent_id, message).await,
        }
    }

    async fn push(
        &self,
        chat_id: &str,
        sender_label: &str,
        prompt: &str,
        agent_id: &str,
        response: &str,
    ) -> Result<()> {
        match self {
            Self::InMemory(store) => {
                store.push(chat_id, sender_label, prompt, agent_id, response);
                Ok(())
            }
            #[cfg(feature = "persistent-context")]
            Self::Persistent(store) => {
                store
                    .push(chat_id, sender_label, prompt, agent_id, response)
                    .await
            }
        }
    }

    async fn clear(&self, chat_id: &str) -> Result<()> {
        match self {
            Self::InMemory(store) => {
                store.clear(chat_id);
                Ok(())
            }
            #[cfg(feature = "persistent-context")]
            Self::Persistent(store) => store.clear(chat_id).await,
        }
    }

    async fn exchange_count(&self, chat_id: &str) -> Result<usize> {
        match self {
            Self::InMemory(store) => Ok(store.exchange_count(chat_id)),
            #[cfg(feature = "persistent-context")]
            Self::Persistent(store) => store.exchange_count(chat_id).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unified_store_creation() -> Result<()> {
        // Test in-memory store creation
        let in_memory_store = UnifiedContextStore::new(20, 5, None).await?;
        assert!(matches!(in_memory_store, UnifiedContextStore::InMemory(_)));

        Ok(())
    }

    #[tokio::test]
    async fn test_unified_store_operations() -> Result<()> {
        // Test with in-memory store
        let store = UnifiedContextStore::new(20, 5, None).await?;

        // Push an exchange
        store
            .push("chat:1", "Brian", "Hello", "librarian", "Hi!")
            .await?;

        // Augment message for new agent
        let augmented = store
            .augment_message("chat:1", "custodian", "New message")
            .await?;
        assert!(augmented.contains("Hello"));
        assert!(augmented.contains("New message"));

        // Clear chat
        store.clear("chat:1").await?;

        Ok(())
    }

    #[cfg(feature = "persistent-context")]
    #[tokio::test]
    async fn test_persistent_store_creation() -> Result<()> {
        use tempfile::NamedTempFile;

        let temp_file = NamedTempFile::new()?;
        let persistent_config = crate::config::PersistentContextConfig {
            database_url: format!("sqlite://{}", temp_file.path().to_string_lossy()),
        };

        let persistent_store = UnifiedContextStore::new(20, 5, Some(&persistent_config)).await?;
        assert!(matches!(
            persistent_store,
            UnifiedContextStore::Persistent(_)
        ));

        Ok(())
    }
}
