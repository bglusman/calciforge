//! PolyClaw v2 — Outpost external content scanning module.
//!
//! Provides injection-resistant scanning of external content before it reaches
//! the model context. Three-layer defence: structural → semantic → HTTP service.
//!
//! # Architecture
//!
//! ```text
//! [External source] → [OutpostProxy::fetch] → [OutpostScanner] → [OutpostVerdict]
//!                                                      ↓
//!                                             [DigestStore]
//!                                              (cache hit?)
//!                                                  ↓ no
//!                                         [OutpostMiddleware]
//!                                                  ↓
//!                                   Clean  → OutpostFetchResult::Ok
//!                                   Review → OutpostFetchResult::Review (with annotation)
//!                                   Unsafe → OutpostFetchResult::Blocked (content withheld)
//! ```
//!
//! # Transparent proxy
//!
//! All external content access MUST go through [`proxy::OutpostProxy::fetch`].
//! Tools never hold raw HTTP clients or touch raw external content directly.
//! The proxy fetches, hashes, checks the [`digest::DigestStore`] cache, and
//! only rescans when the content digest has changed.
//!
//! # Tool deprecation note
//!
//! `web_fetch` and `safe_fetch` were previously separate tools with different
//! safety semantics. With all fetches routed through [`proxy::OutpostProxy`]
//! they are now equivalent — every fetch is a safe fetch. `safe_fetch` is kept
//! in the intercepted-tools list for backwards compatibility but is considered
//! **deprecated**; callers should consolidate on `web_fetch`.

pub mod audit;
pub mod digest;
pub mod middleware;
pub mod patterns;
pub mod proxy;
pub mod scanner;
pub mod verdict;

pub use audit::AuditLogger;
pub use digest::{sha256_hex, ContentDigest, DigestStore};
pub use middleware::{HookOutcome, OutpostMiddleware, ToolHook, ToolResult};
pub use proxy::{OutpostFetchResult, OutpostProxy};
pub use scanner::{OutpostScanner, ScannerConfig};
pub use verdict::{OutpostVerdict, ScanContext};
