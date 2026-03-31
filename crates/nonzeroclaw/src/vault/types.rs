//! Shared value types for the vault subsystem.

use std::time::{Duration, Instant};

/// A retrieved secret value (opaque string).
///
/// Implements `Drop` to zero the underlying buffer, though Rust does not
/// guarantee that the compiler won't copy the buffer before dropping.
/// For higher-assurance wiping, integrate `zeroize` in a future PR.
///
/// Intentionally does NOT implement `Clone` — cloning would create a copy
/// of the secret that won't be zeroed when the original is dropped.
/// If shared ownership is needed, use `Arc<Secret>`.
pub struct Secret {
    /// The secret value as a UTF-8 string.
    value: String,
    /// Human-readable key name this secret was fetched for.
    pub key: String,
}

impl Secret {
    /// Create a new `Secret`.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Expose the raw secret value.
    ///
    /// Call sites should avoid holding the returned reference beyond the
    /// immediate use (e.g. pass to an HTTP header, then drop).
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret {{ key: {:?}, value: [REDACTED] }}", self.key)
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Zero the bytes in-place.  This is a best-effort measure; the compiler
        // may already have copied the data.  Integrate `zeroize` for production.
        for b in unsafe { self.value.as_bytes_mut() } {
            *b = 0;
        }
    }
}

/// The value to store in the vault.
///
/// Intentionally does NOT implement `Clone` — same rationale as `Secret`.
pub struct SecretValue {
    inner: String,
}

impl SecretValue {
    /// Create a new `SecretValue` from a plaintext string.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            inner: value.into(),
        }
    }

    /// Expose the raw value.
    pub fn expose(&self) -> &str {
        &self.inner
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretValue([REDACTED])")
    }
}

/// A short-lived vault session token returned by `VaultAdapter::unlock`.
///
/// Intentionally does NOT implement `Clone` — cloning would create an
/// untracked copy of the session token. Use `expose()` to get the string
/// if you need to pass the token to a subprocess.
pub struct SessionToken {
    pub(crate) token: String,
    pub(crate) obtained_at: Instant,
    pub(crate) ttl: Duration,
}

impl SessionToken {
    /// Create a new session token.
    pub fn new(token: impl Into<String>, ttl: Duration) -> Self {
        Self {
            token: token.into(),
            obtained_at: Instant::now(),
            ttl,
        }
    }

    /// Return true if this token is still valid.
    pub fn is_valid(&self) -> bool {
        self.obtained_at.elapsed() < self.ttl
    }

    /// Expose the raw token string (for use as `BW_SESSION` env var).
    pub fn expose(&self) -> &str {
        &self.token
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SessionToken {{ valid: {}, age: {:?} }}",
            self.is_valid(),
            self.obtained_at.elapsed()
        )
    }
}

/// Per-secret approval policy.
///
/// Controls when (and if) a human operator must approve before the vault
/// releases a secret to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretPolicy {
    /// Inject silently — no human approval required.
    Auto,

    /// Require explicit approval on every access.
    PerUse,

    /// Approve once per agent session; cache the decision for the session lifetime.
    Session,

    /// Approve once; the decision is valid for `ttl` duration.
    TimeBound { ttl: Duration },
}

impl Default for SecretPolicy {
    fn default() -> Self {
        Self::Auto
    }
}
