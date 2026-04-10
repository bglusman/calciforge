//! clashd - Policy sidecar for OpenClaw
//!
//! Evaluates tool calls against Starlark policies with domain filtering
//! and per-agent policy scoping.

pub mod domain_lists;
pub mod policy;

// Re-export main types for convenience
pub use domain_lists::DomainListManager;
pub use policy::{AgentPolicyConfig, DomainListSource, PolicyEngine, PolicyResult, Verdict};
