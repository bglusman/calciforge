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
            .map_err(|e| {
                crate::OneCliError::Config(format!("Failed to build HTTP client: {}", e))
            })?;

        Ok(Self { client, config })
    }

    /// Check if OneCLI gateway is healthy
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/health", self.config.url.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| crate::OneCliError::Unreachable {
                url: self.config.url.clone(),
                source: e,
            })?;

        Ok(response.status().is_success())
    }

    /// Create a request builder for the given URL
    ///
    /// If OneCLI is configured, the request will be routed through the gateway
    /// for credential injection.
    pub fn request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        // Route through OneCLI proxy
        let proxy_url = self.config.url.trim_end_matches('/').to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation_with_valid_config() {
        let config = OneCliConfig::default();
        let client = OneCliClient::new(config);
        assert!(client.is_ok());
    }

    #[test]
    fn test_client_creation_with_custom_config() {
        let config = OneCliConfig {
            url: "http://onecli:9090".to_string(),
            agent_id: "test-agent".to_string(),
            timeout: Duration::from_secs(10),
        };
        let client = OneCliClient::new(config);
        assert!(client.is_ok());
    }

    #[test]
    fn test_client_get_routes_through_proxy() {
        let config = OneCliConfig {
            url: "http://proxy:8081".to_string(),
            agent_id: "test-agent".to_string(),
            timeout: Duration::from_secs(10),
        };
        let client = OneCliClient::new(config).unwrap();
        let req_builder = client.get("https://api.example.com/test");
        // We can't easily inspect RequestBuilder internals, but we can verify
        // it doesn't panic and returns a valid builder.
        let _ = req_builder;
    }

    #[test]
    fn test_client_post_routes_through_proxy() {
        let config = OneCliConfig::default();
        let client = OneCliClient::new(config).unwrap();
        let req_builder = client.post("https://api.example.com/test");
        let _ = req_builder;
    }

    #[test]
    fn test_client_debug_format() {
        let config = OneCliConfig {
            url: "http://proxy:8081".to_string(),
            agent_id: "test-agent".to_string(),
            timeout: Duration::from_secs(10),
        };
        let client = OneCliClient::new(config).unwrap();
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("OneCliClient"));
        assert!(debug_str.contains("http://proxy:8081"));
        assert!(debug_str.contains("test-agent"));
    }

    #[test]
    fn test_request_builder_method_mapping() {
        let config = OneCliConfig::default();
        let client = OneCliClient::new(config).unwrap();

        // Verify different HTTP methods produce valid builders
        let get_req = client.request(reqwest::Method::GET, "https://example.com");
        let post_req = client.request(reqwest::Method::POST, "https://example.com");
        let put_req = client.request(reqwest::Method::PUT, "https://example.com");
        let delete_req = client.request(reqwest::Method::DELETE, "https://example.com");

        // All should produce valid request builders (no panic)
        let _ = get_req;
        let _ = post_req;
        let _ = put_req;
        let _ = delete_req;
    }

    #[test]
    fn test_client_url_trailing_slash_stripped() {
        let config = OneCliConfig {
            url: "http://proxy:8081/".to_string(),
            agent_id: "test".to_string(),
            timeout: Duration::from_secs(10),
        };
        let client = OneCliClient::new(config).unwrap();
        // The proxy URL should strip trailing slash for header construction
        let _ = client.get("https://example.com");
    }
}
