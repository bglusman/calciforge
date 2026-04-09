//! clashd - Policy sidecar for OpenClaw
//!
//! Evaluates tool calls against Starlark policies.

pub mod policy;

// Re-export main types for convenience
pub use policy::{PolicyResult, Verdict};