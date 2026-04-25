//! OneCLI Client Retry Logic

use super::error::{OneCliError, Result};
use crate::config::RetryConfig;
use std::future::Future;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

/// A trait for strategies that determine if a request should be retried.
pub trait RetryStrategy: Send + Sync + 'static {
    /// Determines if the given error is retryable.
    fn is_retryable(&self, error: &OneCliError) -> bool;
}

/// Default retry strategy for OneCLI client.
#[derive(Debug, Clone, Default)]
pub struct DefaultRetryStrategy;

impl RetryStrategy for DefaultRetryStrategy {
    fn is_retryable(&self, error: &OneCliError) -> bool {
        error.is_retryable()
    }
}

/// Execute a fallible operation with retry logic.
pub async fn execute_with_retry<F, Fut, T>(
    config: &RetryConfig,
    strategy: impl RetryStrategy,
    mut operation: F,
) -> Result<T>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<T>> + Send,
{
    let mut attempts = 0;
    let max_retries = config.max_retries;
    let mut backoff_ms = config.base_delay.as_millis() as u64;
    let max_delay_ms = config.max_delay.as_millis() as u64;

    loop {
        attempts += 1;

        match operation().await {
            Ok(val) => {
                if attempts > 1 {
                    info!("Operation succeeded after {} retries.", attempts - 1);
                }
                return Ok(val);
            }
            Err(e) => {
                if !strategy.is_retryable(&e) || attempts > max_retries {
                    error!(
                        "Attempt {} failed (not retryable or max retries reached): {}",
                        attempts, e
                    );
                    return Err(e);
                }

                warn!(
                    "Attempt {} failed (retryable), retrying in {}ms: {}",
                    attempts, backoff_ms, e
                );

                sleep(Duration::from_millis(backoff_ms)).await;

                // Exponential backoff with 2x multiplier
                backoff_ms = (backoff_ms * 2).min(max_delay_ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_default_retry_strategy() {
        let strategy = DefaultRetryStrategy;

        assert!(strategy.is_retryable(&OneCliError::RateLimited { retry_after: 5 }));
        assert!(!strategy.is_retryable(&OneCliError::PolicyDenied("test".to_string())));
        assert!(!strategy.is_retryable(&OneCliError::CredentialNotFound("test".to_string())));
    }

    #[tokio::test]
    async fn test_execute_with_retry_success_first_attempt() {
        let config = RetryConfig::default();
        let strategy = DefaultRetryStrategy;
        let counter = AtomicUsize::new(0);

        let result = execute_with_retry(&config, strategy, || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Ok::<i32, OneCliError>(100) }
        })
        .await
        .unwrap();

        assert_eq!(result, 100);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_execute_with_retry_max_retries_exceeded() {
        let config = RetryConfig::default(); // max_retries = 3
        let strategy = DefaultRetryStrategy;
        let counter = AtomicUsize::new(0);

        let result = execute_with_retry(&config, strategy, || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Err::<i32, OneCliError>(OneCliError::RateLimited { retry_after: 5 }) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            counter.load(Ordering::SeqCst),
            config.max_retries as usize + 1
        );
    }

    /// Given an operation that fails twice then succeeds,
    /// when execute_with_retry runs (with base_delay=1ms so the test
    /// finishes quickly),
    /// then the operation was called exactly three times and the
    /// final result is the success value.
    ///
    /// Catches a regression where the retry loop short-circuits on
    /// first success but fails to propagate the value, or re-attempts
    /// after success.
    #[tokio::test]
    async fn retries_until_transient_resolves() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let counter = AtomicUsize::new(0);

        let result = execute_with_retry(&config, DefaultRetryStrategy, || {
            let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if attempt < 3 {
                    Err::<i32, OneCliError>(OneCliError::RateLimited { retry_after: 0 })
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .expect("should succeed on attempt 3");

        assert_eq!(result, 42, "return value from the successful attempt");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "attempts 1 & 2 failed transient; attempt 3 succeeded"
        );
    }

    /// Given an operation that fails with a non-retryable error,
    /// when execute_with_retry runs,
    /// then the operation is called exactly ONCE and the error
    /// propagates — no retries, regardless of max_retries.
    ///
    /// Catches a regression where the retry loop ignores the
    /// strategy's verdict and retries everything unconditionally.
    #[tokio::test]
    async fn non_retryable_error_aborts_immediately() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let counter = AtomicUsize::new(0);

        let result = execute_with_retry(&config, DefaultRetryStrategy, || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Err::<i32, OneCliError>(OneCliError::PolicyDenied("no".to_string())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "non-retryable error must not retry, even when max_retries > 0"
        );
    }
}
