//! Memory hooks module.
//!
//! Provides the `PreReadHook` and `PostWriteHook` traits defined in the
//! ZeroClawed v2 spec (Section 8), plus no-op default implementations.

pub mod memory;

pub use memory::{
    CompletedTurn, InboundMessage, MemoryChunk, MemoryEntry, MemoryStore, NoOpPostWriteHook,
    NoOpPreReadHook, PostWriteHook, PreReadHook, WriteDecision,
};
