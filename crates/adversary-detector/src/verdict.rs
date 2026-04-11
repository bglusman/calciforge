//! Core verdict and context types for adversary scanning.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The result of a security scan.
///
/// Determines how tool output is handled before it reaches the model context.
/// `Unsafe` content must never be returned to the model — the middleware returns
/// an error string instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum ScanVerdict {
    /// Content passed all checks. Pass through to the model unchanged.
    Clean,
    /// Content is ambiguous or mildly suspicious. Pass through with a warning annotation.
    Review {
        /// Human-readable reason for the review flag.
        reason: String,
    },
    /// Content failed scanning and is blocked. Never return actual content to the model.
    Unsafe {
        /// Human-readable reason for the block.
        reason: String,
    },
}

impl ScanVerdict {
    /// Returns `true` if this verdict is [`ScanVerdict::Clean`].
    pub fn is_clean(&self) -> bool {
        matches!(self, ScanVerdict::Clean)
    }

    /// Returns `true` if this verdict is [`ScanVerdict::Unsafe`].
    pub fn is_unsafe(&self) -> bool {
        matches!(self, ScanVerdict::Unsafe { .. })
    }

    /// Returns the reason string, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            ScanVerdict::Clean => None,
            ScanVerdict::Review { reason } | ScanVerdict::Unsafe { reason } => {
                Some(reason.as_str())
            }
        }
    }

    /// Returns the short verdict name for logging and serialization.
    pub fn name(&self) -> &'static str {
        match self {
            ScanVerdict::Clean => "clean",
            ScanVerdict::Review { .. } => "review",
            ScanVerdict::Unsafe { .. } => "unsafe",
        }
    }
}

impl fmt::Display for ScanVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanVerdict::Clean => write!(f, "clean"),
            ScanVerdict::Review { reason } => write!(f, "review({})", reason),
            ScanVerdict::Unsafe { reason } => write!(f, "unsafe({})", reason),
        }
    }
}

/// The tool context in which a scan is being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanContext {
    /// Response body from an HTTP fetch.
    WebFetch,
    /// Result snippets from a search API.
    WebSearch,
    /// Email body or subject.
    Email,
    /// stdout/stderr from a shell command.
    Exec,
    /// Response body from a third-party API call.
    Api,
    /// Inbound user message arriving via channel.
    UserMessage,
}

impl ScanContext {
    /// Returns the canonical string name used in audit logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ScanContext::WebFetch => "web_fetch",
            ScanContext::WebSearch => "web_search",
            ScanContext::Email => "email_fetch",
            ScanContext::Exec => "exec",
            ScanContext::Api => "api",
            ScanContext::UserMessage => "user_message",
        }
    }
}

impl fmt::Display for ScanContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
