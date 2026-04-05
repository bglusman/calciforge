//! OneCLI Client
//!
//! HTTP client that routes requests through OneCLI gateway for credential injection.

use crate::{OneCliConfig, Result};
use reqwest::{Client, RequestBuilder};
use std::time::Duration;

/// OneCLI HTTP client
#[derive(Clone)]
pub struct OneCliClient {
    client: Client,
    config: OneCliConfig,
}

impl OneCliClient {
    /// Create a new OneCLI client
    pub fn new(config: OneCliConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| crate::OneCliError::Config(format!("Failed to build HTTP client: {}", e)))?;
        
        Ok(Self { client, config })
    }

    /// Check if OneCLI gateway is healthy
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/health", self.config.url.trim_end_matches('/'));
        let response = self.client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| crate::OneCliError::Unreachable { 
                url: self.config.url.clone(), 
                source: e 
            })?;
        
        Ok(response.status().is_success())
    }

    /// Create a request builder for the given URL
    /// 
    /// If OneCLI is configured, the request will be routed through the gateway
    /// for credential injection.
    pub fn request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        // Route through OneCLI proxy
        let proxy_url = format!("{}", self.config.url.trim_end_matches('/'));
        self.client
            .request(method, &proxy_url)
            .header("X-Target-URL", url)
            .header("X-OneCLI-Agent-ID", &self.config.agent_id)
    }

    /// GET request
    pub fn get(&self, url: &str) -> RequestBuilder {
        self.request(reqwest::Method::GET, url)
    }

    /// POST request
    pub fn post(&self, url: &str) -> RequestBuilder {
        self.request(reqwest::Method::POST, url)
    }
}

impl std::fmt::Debug for OneCliClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OneCliClient")
            .field("url", &self.config.url)
            .field("agent_id", &self.config.agent_id)
            .finish()
    }
}
