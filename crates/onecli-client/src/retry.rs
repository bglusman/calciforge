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
        let strategy = DefaultRetryStrategy::default();

        assert!(strategy.is_retryable(&OneCliError::Unreachable { 
            url: "test".to_string(), 
            source: reqwest::Error::from(std::io::Error::new(std::io::ErrorKind::Other, "test")) 
        }));
        
        assert!(!strategy.is_retryable(&OneCliError::PolicyDenied("test".to_string())));
        assert!(!strategy.is_retryable(&OneCliError::CredentialNotFound("test".to_string())));
    }

    #[tokio::test]
    async fn test_execute_with_retry_success_first_attempt() {
        let config = RetryConfig::default();
        let strategy = DefaultRetryStrategy::default();
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
        let strategy = DefaultRetryStrategy::default();
        let counter = AtomicUsize::new(0);

        let result = execute_with_retry(&config, strategy, || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                Err::<i32, OneCliError>(OneCliError::Unreachable {
                    url: "test".to_string(),
                    source: reqwest::Error::from(std::io::Error::new(std::io::ErrorKind::Other, "test"))
                })
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), config.max_retries as usize + 1);
    }
}
