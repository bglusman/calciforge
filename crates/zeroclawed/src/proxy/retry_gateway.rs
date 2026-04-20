use std::sync::Arc;

use async_trait::async_trait;
use backon::{ExponentialBuilder, Retryable};
use tracing::{info, warn};

use crate::proxy::gateway::{BackendError, ChatCompletionRequest, ChatCompletionResponse, Gateway, ModelInfo};

use super::retry::RetryConfig;

pub struct RetryGateway {
    inner: Arc<dyn Gateway>,
    config: RetryConfig,
}

impl RetryGateway {
    pub fn new(inner: Arc<dyn Gateway>, config: RetryConfig) -> Self {
        Self { inner, config }
    }
}

#[async_trait]
impl Gateway for RetryGateway {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, BackendError> {
        info!("RetryGateway: Starting chat completion with retry config: {:?}", self.config);
        
        let inner = self.inner.clone();
        let retry_config = self.config.clone();
        
        // Create retryable operation
        let operation = || async {
            let result = inner.chat_completion(request.clone()).await;
            
            // Check if we should retry
            match &result {
                Ok(_) => {
                    info!("RetryGateway: Request succeeded");
                    result
                }
                Err(e) => {
                    // Retry on 5xx errors and connection errors
                    let should_retry = match e {
                        BackendError::HttpError(status) => {
                            status.is_server_error() || *status == 429 // rate limit
                        }
                        BackendError::NetworkError(_) => true,
                        BackendError::TimeoutError => true,
                        BackendError::BackendError(_) => true,
                        _ => false, // Don't retry on 4xx errors (client errors)
                    };
                    
                    if should_retry {
                        warn!("RetryGateway: Retryable error: {}", e);
                    } else {
                        warn!("RetryGateway: Non-retryable error: {}", e);
                    }
                    
                    result
                }
            }
        };
        
        // Apply retry based on config
        match retry_config {
            RetryConfig::Exponential { min_delay, max_delay, max_retries, factor } => {
                let retry_policy = ExponentialBuilder::default()
                    .with_min_delay(min_delay)
                    .with_max_delay(max_delay)
                    .with_max_times(max_retries as usize)
                    .with_factor(factor)
                    .with_jitter();
                
                operation.retry(&retry_policy).await
            }
            RetryConfig::Constant { delay, max_retries } => {
                let retry_policy = ExponentialBuilder::default()
                    .with_delay(delay)
                    .with_max_times(max_retries as usize)
                    .with_jitter();
                
                operation.retry(&retry_policy).await
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // No retry for list_models (usually cached)
        self.inner.list_models().await
    }
}