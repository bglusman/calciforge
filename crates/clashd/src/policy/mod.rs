//! Policy engine for clashd
//!
//! Evaluates tool calls against Starlark policies to determine
//! whether operations should be allowed, denied, or require review.

pub mod engine;
pub mod eval;

pub use engine::{AgentPolicyConfig, DomainListSource, PolicyEngine};

use serde::{Deserialize, Serialize};

/// The verdict of a policy evaluation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Operation is permitted
    Allow,
    /// Operation requires human review
    Review,
    /// Operation is blocked
    Deny,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Allow => write!(f, "allow"),
            Verdict::Review => write!(f, "review"),
            Verdict::Deny => write!(f, "deny"),
        }
    }
}

/// Result of a policy evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyResult {
    /// The verdict
    pub verdict: Verdict,
    /// Optional explanation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PolicyResult {
    /// Create an allow result
    pub fn allow() -> Self {
        Self {
            verdict: Verdict::Allow,
            reason: None,
        }
    }

    /// Create a deny result with reason
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Deny,
            reason: Some(reason.into()),
        }
    }

    /// Create a review result with reason
    pub fn review(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Review,
            reason: Some(reason.into()),
        }
    }
}
