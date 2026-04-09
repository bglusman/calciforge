//! Memory hook traits and no-op implementations.
//!
//! From the ZeroClawed spec, Section 8.2:
//! > The hooks are **optional traits** — if no implementation is configured,
//! > the hook is a no-op and the system behaves as today. When configured,
//! > the hook is enforced regardless of the main agent's internal decisions.
//!
//! These types are designed to be swappable — you can replace NoOp with an
//! embedding-based or LLM-based implementation without changing any call sites.

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// Core message types
// ---------------------------------------------------------------------------

/// An inbound message arriving on a channel.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// The resolved identity ID of the sender.
    pub _sender_id: String,
    /// The channel this message arrived on (e.g. "telegram", "signal").
    pub _channel: String,
    /// The text content of the message.
    pub _text: String,
    /// Optional thread/conversation ID.
    pub _thread_id: Option<String>,
}

/// A chunk of memory to inject into context before the agent sees the message.
#[derive(Debug, Clone)]
pub struct MemoryChunk {
    /// Unique identifier for this chunk (e.g. SQLite row ID, vector DB ID).
    pub id: String,
    /// The text content of the memory chunk.
    pub _content: String,
    /// Optional relevance score (0.0–1.0) from a similarity search.
    pub score: Option<f32>,
}

/// A completed turn: inbound message + agent response + any metadata.
#[derive(Debug, Clone)]
pub struct CompletedTurn {
    /// The original inbound message.
    pub _message: InboundMessage,
    /// The agent's response text.
    pub _response: String,
    /// Duration of the turn in milliseconds (for latency tracking).
    pub _duration_ms: u64,
}

/// An entry to be written to the memory store.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    /// The text content to persist.
    pub _content: String,
    /// Optional category/tag (e.g. "fact", "preference", "event").
    pub category: Option<String>,
    /// The identity ID this memory is associated with.
    pub _identity_id: Option<String>,
}

/// Decision returned by a `PostWriteHook`.
#[derive(Debug, Clone)]
pub enum WriteDecision {
    /// Nothing worth recording from this turn.
    Skip,
    /// Record these entries in the pending buffer.
    Write(Vec<MemoryEntry>),
}

// ---------------------------------------------------------------------------
// MemoryStore trait
// ---------------------------------------------------------------------------

/// Abstract handle to the memory backing store.
///
/// Implementations can be in-memory, SQLite, or an HTTP vector DB.
/// Not yet implemented for Phase 1 (hooks use it but the no-op impls ignore it).
#[async_trait]
pub trait MemoryStore: Send + Sync {}

// ---------------------------------------------------------------------------
// PreReadHook trait
// ---------------------------------------------------------------------------

/// Pre-turn retrieval hook.
///
/// Called before the main agent sees an inbound message. The hook may inject
/// additional context (retrieved memory chunks) into the message before it
/// reaches the model.
///
/// Returning an empty Vec means "nothing to inject" — a valid response.
#[async_trait]
pub trait PreReadHook: Send + Sync {
    /// Given the inbound message and a handle to the memory store,
    /// return zero or more memory chunks to inject into context.
    async fn evaluate(&self, message: &InboundMessage, store: &dyn MemoryStore)
        -> Vec<MemoryChunk>;
}

// ---------------------------------------------------------------------------
// PostWriteHook trait
// ---------------------------------------------------------------------------

/// Post-turn write hook.
///
/// Called after the main agent has produced a response, before it is dispatched
/// to the user. The hook may persist memory from the turn.
#[async_trait]
pub trait PostWriteHook: Send + Sync {
    /// Given the completed turn (message + response), optionally persist memory.
    /// Writes go to the pending buffer; a background consolidation job compacts
    /// and deduplicates periodically.
    async fn evaluate(&self, turn: &CompletedTurn, store: &dyn MemoryStore) -> WriteDecision;
}

// ---------------------------------------------------------------------------
// No-op implementations (Phase 1 defaults)
// ---------------------------------------------------------------------------

/// No-op pre-read hook. Returns empty Vec always, zero cost.
///
/// This is the default when no hook is configured — the system behaves
/// identically to having no memory hooks at all.
pub struct NoOpPreReadHook;

#[async_trait]
impl PreReadHook for NoOpPreReadHook {
    async fn evaluate(
        &self,
        _message: &InboundMessage,
        _store: &dyn MemoryStore,
    ) -> Vec<MemoryChunk> {
        vec![]
    }
}

/// No-op post-write hook. Returns Skip always, zero cost.
///
/// This is the default when no hook is configured — nothing is persisted.
pub struct NoOpPostWriteHook;

#[async_trait]
impl PostWriteHook for NoOpPostWriteHook {
    async fn evaluate(&self, _turn: &CompletedTurn, _store: &dyn MemoryStore) -> WriteDecision {
        WriteDecision::Skip
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal in-memory store for testing hooks.
    struct InMemoryStore {
        _chunks: Vec<MemoryChunk>,
    }

    impl InMemoryStore {
        fn empty() -> Self {
            Self { _chunks: vec![] }
        }

        fn with_chunks(chunks: Vec<MemoryChunk>) -> Self {
            Self { _chunks: chunks }
        }
    }

    impl MemoryStore for InMemoryStore {}

    fn make_message() -> InboundMessage {
        InboundMessage {
            _sender_id: "brian".to_string(),
            _channel: "telegram".to_string(),
            _text: "What did we discuss yesterday?".to_string(),
            _thread_id: None,
        }
    }

    fn make_turn() -> CompletedTurn {
        CompletedTurn {
            _message: make_message(),
            _response: "We discussed the Proxmox cluster setup.".to_string(),
            _duration_ms: 1200,
        }
    }

    #[tokio::test]
    async fn test_noop_pre_read_returns_empty() {
        let hook = NoOpPreReadHook;
        let store = InMemoryStore::empty();
        let chunks = hook.evaluate(&make_message(), &store).await;
        assert!(chunks.is_empty(), "NoOp should return empty chunks");
    }

    #[tokio::test]
    async fn test_noop_pre_read_ignores_store_contents() {
        let hook = NoOpPreReadHook;
        let store = InMemoryStore::with_chunks(vec![MemoryChunk {
            id: "1".to_string(),
            _content: "Yesterday we talked about X".to_string(),
            score: Some(0.95),
        }]);
        // Even with a populated store, NoOp returns nothing
        let chunks = hook.evaluate(&make_message(), &store).await;
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn test_noop_post_write_returns_skip() {
        let hook = NoOpPostWriteHook;
        let store = InMemoryStore::empty();
        let decision = hook.evaluate(&make_turn(), &store).await;
        assert!(
            matches!(decision, WriteDecision::Skip),
            "NoOp should return Skip"
        );
    }

    #[tokio::test]
    async fn test_noop_hooks_are_send_sync() {
        // Compile-time check: hooks can be used behind Arc in async contexts
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoOpPreReadHook>();
        assert_send_sync::<NoOpPostWriteHook>();
    }

    #[tokio::test]
    async fn test_memory_chunk_fields() {
        let chunk = MemoryChunk {
            id: "chunk-1".to_string(),
            _content: "Brian prefers concise responses.".to_string(),
            score: Some(0.87),
        };
        assert_eq!(chunk.id, "chunk-1");
        assert_eq!(chunk.score, Some(0.87));
    }

    #[tokio::test]
    async fn test_write_decision_variant() {
        let entries = vec![MemoryEntry {
            _content: "Discussed Proxmox upgrade".to_string(),
            category: Some("event".to_string()),
            _identity_id: Some("brian".to_string()),
        }];
        let decision = WriteDecision::Write(entries.clone());
        if let WriteDecision::Write(written) = decision {
            assert_eq!(written.len(), 1);
            assert_eq!(written[0].category, Some("event".to_string()));
        } else {
            panic!("expected Write variant");
        }
    }
}
