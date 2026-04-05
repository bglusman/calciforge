//! OneCLI Client for ZeroClawed
//! 
//! Provides credential proxy integration with OneCLI Agent Vault.
//! Routes outbound HTTP requests through OneCLI for credential injection
//! and policy enforcement.

pub mod client;
pub mod config;
pub mod error;
pub mod retry;

pub use client::OneCliClient;
pub use config::OneCliConfig;
pub use error::{OneCliError, Result};

use std::sync::Arc;

/// OneCLI integration handle
#[derive(Clone)]
pub struct OneCliIntegration {
    client: Arc<OneCliClient>,
}

impl OneCliIntegration {
    /// Create a new OneCLI integration
    pub fn new(config: OneCliConfig) -> Result<Self> {
        let client = OneCliClient::new(config)?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Get the underlying client
    pub fn client(&self) -> &OneCliClient {
        &self.client
    }

    /// Check if OneCLI is available and healthy
    pub async fn health_check(&self) -> Result<bool> {
        self.client.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integration_creation() {
        let config = OneCliConfig::default();
        let integration = OneCliIntegration::new(config);
        assert!(integration.is_ok());
    }
}
