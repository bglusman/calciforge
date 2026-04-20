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
        
        let is_retryable = |e: &BackendError| match e {
            BackendError::HttpError(status) => status.is_server_error() || *status == 429,
            BackendError::NetworkError(_) => true,
            BackendError::TimeoutError => true,
            BackendError::BackendError(_) => true,
            _ => false,
        };

        let operation = || async {
            let result = inner.chat_completion(request.clone()).await;
            match &result {
                Ok(_) => info!("RetryGateway: Request succeeded"),
                Err(e) if is_retryable(e) => warn!("RetryGateway: Retryable error: {}", e),
                Err(e) => warn!("RetryGateway: Non-retryable error: {}", e),
            }
            result
        };

        match retry_config {
            RetryConfig::Exponential { min_delay, max_delay, max_retries, factor } => {
                let policy = ExponentialBuilder::default()
                    .with_min_delay(min_delay)
                    .with_max_delay(max_delay)
                    .with_max_times(max_retries as usize)
                    .with_factor(factor)
                    .with_jitter();
                operation.retry_if(&policy, is_retryable).await
            }
            RetryConfig::Constant { delay, max_retries } => {
                let policy = ExponentialBuilder::default()
                    .with_delay(delay)
                    .with_max_times(max_retries as usize)
                    .with_jitter();
                operation.retry_if(&policy, is_retryable).await
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // No retry for list_models (usually cached)
        self.inner.list_models().await
    }
}