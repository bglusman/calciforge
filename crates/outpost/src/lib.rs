//! PolyClaw v2 — Outpost external content scanning module.
//!
//! Provides injection-resistant scanning of external content before it reaches
//! the model context. Three-layer defence: structural → semantic → HTTP service.
//!
//! # Architecture
//!
//! ```text
//! [External source] → [fetch/exec tool] → [OutpostScanner] → [OutpostVerdict]
//!                                                  ↓
//!                                         [OutpostMiddleware]
//!                                                  ↓
//!                                   Clean → pass through
//!                                   Review → prepend warning
//!                                   Unsafe → block entirely
//! ```

pub mod audit;
pub mod middleware;
pub mod patterns;
pub mod scanner;
pub mod verdict;

pub use audit::AuditLogger;
pub use middleware::{HookOutcome, OutpostMiddleware, ToolHook, ToolResult};
pub use scanner::{OutpostScanner, ScannerConfig};
pub use verdict::{OutpostVerdict, ScanContext};
