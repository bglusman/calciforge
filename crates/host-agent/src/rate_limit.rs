//! Per-CN token-bucket rate limiter for destructive endpoints (P-B5)
//!
//! Implements a simple, cooperative in-process token-bucket limiter using
//! DashMap (lock-striped concurrent HashMap) so it never blocks the async runtime.
//!
//! # Defaults
//! - 5 requests per 60-second window per CN
//! - Applied to: /zfs/destroy, /approve, /pending
//! - On rate-limit: increments `rate_limited` counter and returns HTTP 429.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::warn;

use crate::config::RateLimitConfig;

/// Token bucket state per CN
#[derive(Debug)]
struct Bucket {
    /// Number of requests used in the current window
    count: u64,
    /// When the current window started
    window_start: Instant,
}

/// Per-CN token bucket rate limiter
///
/// Thread-safe via DashMap — safe for concurrent access from multiple Tokio tasks
/// without holding a global lock. Each CN gets its own shard in DashMap.
pub struct RateLimiter {
    /// Map from CN to bucket state
    buckets: DashMap<String, Bucket>,
    /// Config snapshot (cloned at startup, reloadable via `update_config`)
    max_requests: u32,
    window: Duration,
    enabled: bool,
    /// Endpoints subject to rate limiting (set membership check)
    endpoints: Vec<String>,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            buckets: DashMap::new(),
            max_requests: config.max_requests,
            window: Duration::from_secs(config.window_seconds),
            enabled: config.enabled,
            endpoints: config.endpoints.clone(),
        }
    }

    /// Returns true if this endpoint is subject to rate limiting.
    pub fn applies_to(&self, path: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.endpoints.iter().any(|e| e == path)
    }

    /// Check and consume one token for the given CN on a rate-limited endpoint.
    ///
    /// Returns `Ok(())` if the request is allowed, or `Err(RetryAfter)` with
    /// the seconds until the window resets if it is denied.
    pub fn check(&self, cn: &str) -> Result<(), u64> {
        if !self.enabled {
            return Ok(());
        }

        let now = Instant::now();

        // DashMap entry API gives us per-key locking — no global lock needed.
        let mut entry = self
            .buckets
            .entry(cn.to_string())
            .or_insert_with(|| Bucket {
                count: 0,
                window_start: now,
            });

        let elapsed = now.duration_since(entry.window_start);

        if elapsed >= self.window {
            // New window: reset counter
            entry.window_start = now;
            entry.count = 1;
            Ok(())
        } else if entry.count < self.max_requests as u64 {
            entry.count += 1;
            Ok(())
        } else {
            let retry_after = (self.window - elapsed).as_secs().max(1);
            warn!(
                cn = %cn,
                count = %entry.count,
                max = %self.max_requests,
                retry_after_secs = %retry_after,
                "Rate limit exceeded"
            );
            Err(retry_after)
        }
    }

    /// Remove expired buckets to prevent unbounded growth.
    /// Call this periodically (e.g., every 5 minutes from a background task).
    pub fn evict_expired(&self) {
        let now = Instant::now();
        self.buckets
            .retain(|_, v| now.duration_since(v.window_start) < self.window * 2);
    }
}

/// Axum extractor / middleware helper — returns 429 with Retry-After header on violation.
/// Call from handler or tower layer. Returns human-readable JSON error.
pub fn rate_limit_response(retry_after: u64) -> axum::response::Response {
    use axum::http::{HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    let body = serde_json::json!({
        "success": false,
        "error": "Rate limit exceeded. Too many requests.",
        "retry_after_seconds": retry_after,
    });

    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (
                "Retry-After",
                HeaderValue::from_str(&retry_after.to_string()).unwrap(),
            ),
            ("Content-Type", HeaderValue::from_static("application/json")),
        ],
        axum::Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RateLimitConfig;

    fn test_limiter(max: u32, window_secs: u64) -> RateLimiter {
        RateLimiter::new(&RateLimitConfig {
            enabled: true,
            max_requests: max,
            window_seconds: window_secs,
            endpoints: vec!["/zfs/destroy".to_string()],
        })
    }

    #[test]
    fn test_allows_up_to_limit() {
        let limiter = test_limiter(3, 60);
        assert!(limiter.check("librarian").is_ok());
        assert!(limiter.check("librarian").is_ok());
        assert!(limiter.check("librarian").is_ok());
        // 4th request denied
        assert!(limiter.check("librarian").is_err());
    }

    #[test]
    fn test_separate_cns_independent() {
        let limiter = test_limiter(2, 60);
        assert!(limiter.check("librarian").is_ok());
        assert!(limiter.check("librarian").is_ok());
        assert!(limiter.check("librarian").is_err());

        // lucien is unaffected
        assert!(limiter.check("lucien").is_ok());
        assert!(limiter.check("lucien").is_ok());
        assert!(limiter.check("lucien").is_err());
    }

    #[test]
    fn test_window_reset() {
        let limiter = test_limiter(1, 0); // 0-second window means instant reset
        assert!(limiter.check("librarian").is_ok());
        // Sleep briefly to let the zero-duration window expire
        std::thread::sleep(Duration::from_millis(5));
        assert!(limiter.check("librarian").is_ok());
    }

    #[test]
    fn test_disabled_limiter_always_allows() {
        let limiter = RateLimiter::new(&RateLimitConfig {
            enabled: false,
            max_requests: 1,
            window_seconds: 60,
            endpoints: vec!["/zfs/destroy".to_string()],
        });

        for _ in 0..100 {
            assert!(limiter.check("any-agent").is_ok());
        }
    }

    #[test]
    fn test_applies_to() {
        let limiter = test_limiter(5, 60);
        assert!(limiter.applies_to("/zfs/destroy"));
        assert!(!limiter.applies_to("/zfs/list"));
        assert!(!limiter.applies_to("/health"));
    }

    #[test]
    fn test_retry_after_positive() {
        let limiter = test_limiter(1, 60);
        limiter.check("cn").unwrap();
        let err = limiter.check("cn").unwrap_err();
        assert!(err > 0, "retry_after should be positive");
    }
}
