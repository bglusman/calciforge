//! ZeroClawed — Adversary external content scanning module.
//!
//! Provides injection-resistant scanning of external content before it reaches
//! the model context. Three-layer defense: structural → semantic → HTTP service.
//!
//! # Architecture
//!
//! ```text
//! [External source] → [AdversaryDetector::fetch] → [AdversaryScanner] → [ScanVerdict]
//!                                                      ↓
//!                                             [DigestStore]
//!                                              (cache hit?)
//!                                                  ↓ no
//!                                         [ChannelScanner]
//!                                                  ↓
//!                                   Clean  → AdversaryFetchResult::Ok
//!                                   Review → AdversaryFetchResult::Review (with annotation)
//!                                   Unsafe → AdversaryFetchResult::Blocked (content withheld)
//! ```
//!
//! # Transparent proxy
//!
//! All external content access MUST go through [`proxy::AdversaryDetector::fetch`].
//! Tools never hold raw HTTP clients or touch raw external content directly.
//! The proxy fetches, hashes, checks the [`digest::DigestStore`] cache, and
//! only rescans when the content digest has changed.
//!
//! # Tool deprecation note
//!
//! `web_fetch` and `safe_fetch` were previously separate tools with different
//! safety semantics. With all fetches routed through [`proxy::AdversaryDetector`]
//! they are now equivalent — every fetch is a safe fetch. `safe_fetch` is kept
//! in the intercepted-tools list for backwards compatibility but is considered
//! **deprecated**; callers should consolidate on `web_fetch`.

pub mod audit;
pub mod digest;
pub mod middleware;
pub mod patterns;
pub mod profiles;
pub mod proxy;
pub mod scanner;
pub mod verdict;

/// Extract the host from a URL string.
/// Strips scheme (http://, https://), takes the hostname (before first `/`, `:`, or `?`).
/// Returns empty string if the URL has no scheme (rejects bare strings).
pub fn extract_host(url: &str) -> &str {
    let rest = match url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        Some(r) => r,
        None => return "", // no scheme = not a URL
    };
    // Take up to first `:` (port), `/` (path), or `?` (query)
    let end = rest
        .find(':')
        .or_else(|| rest.find('/'))
        .or_else(|| rest.find('?'))
        .unwrap_or(rest.len());
    &rest[..end]
}

pub use audit::AuditLogger;
pub use digest::{sha256_hex, ContentDigest, DigestStore};
pub use middleware::{ChannelScanner, HookOutcome, InterceptedToolSet, ToolHook, ToolResult};
pub use profiles::{RateLimitConfig, SecurityConfig, SecurityProfile};
pub use proxy::{AdversaryDetector, AdversaryFetchResult};
pub use scanner::{AdversaryScanner, ScannerConfig};
pub use verdict::{ScanContext, ScanVerdict};
