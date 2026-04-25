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
    //! Behavioral tests — each test sends a real HTTP request through
    //! `OneCliClient` to a wiremock-backed proxy and asserts on what
    //! the proxy actually saw. Previously this module had seven tests
    //! that never asserted anything (e.g. `let _ = req_builder`) —
    //! replaced with mock-server integration tests per the test-quality
    //! audit on 2026-04-24.
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    /// Given a `OneCliClient` pointed at a wiremock server,
    /// when the client issues a GET against an arbitrary upstream URL,
    /// then the proxy receives the request with `X-Target-URL` set to
    /// that upstream URL and `X-OneCLI-Agent-ID` set to the configured
    /// agent id. This is the core contract of the client: route through
    /// the gateway, pass the target as a header.
    #[tokio::test]
    async fn get_forwards_target_url_and_agent_id_to_proxy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("X-Target-URL", "https://api.example.com/test"))
            .and(header("X-OneCLI-Agent-ID", "agent-alpha"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = OneCliClient::new(OneCliConfig {
            url: server.uri(),
            agent_id: "agent-alpha".into(),
            timeout: Duration::from_secs(5),
        })
        .expect("client should build with valid config");

        let resp = client
            .get("https://api.example.com/test")
            .send()
            .await
            .expect("request reached proxy");
        assert_eq!(resp.status(), 200);
        // wiremock's .expect(1) + drop verifies the single expected call arrived.
    }

    /// As above but for POST — must preserve the method at the proxy.
    #[tokio::test]
    async fn post_preserves_method_through_proxy() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("X-Target-URL", "https://api.example.com/post"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;

        let client = OneCliClient::new(OneCliConfig {
            url: server.uri(),
            agent_id: "a".into(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();

        let resp = client
            .post("https://api.example.com/post")
            .send()
            .await
            .expect("request reached proxy");
        assert_eq!(resp.status(), 202);
    }

    /// Each method variant must propagate the correct HTTP verb to the
    /// proxy. Previously `test_request_builder_method_mapping` only
    /// asserted the builder didn't panic — this version asserts the
    /// proxy actually saw the verb.
    #[tokio::test]
    async fn request_method_is_preserved_for_get_post_put_delete() {
        let server = MockServer::start().await;
        for verb in ["GET", "POST", "PUT", "DELETE"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
        }

        let client = OneCliClient::new(OneCliConfig {
            url: server.uri(),
            agent_id: "method-test".into(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();

        for m in [
            reqwest::Method::GET,
            reqwest::Method::POST,
            reqwest::Method::PUT,
            reqwest::Method::DELETE,
        ] {
            let resp = client
                .request(m.clone(), "https://example.com/whatever")
                .send()
                .await
                .unwrap_or_else(|e| panic!("send failed for {m}: {e}"));
            assert_eq!(resp.status(), 200, "unexpected status for {m}");
        }
    }

    /// Given a proxy URL configured with a trailing slash,
    /// when the client sends a request,
    /// then the request hits the proxy root exactly once — not
    /// `//health`-style double-slash paths that some proxies reject.
    ///
    /// Previously `test_client_url_trailing_slash_stripped` only
    /// exercised the builder without asserting on the wire form.
    #[tokio::test]
    async fn trailing_slash_on_proxy_url_does_not_double_up_path() {
        let server = MockServer::start().await;
        // Matches exactly one leading slash (the request-target "/")
        // via path("/") — wiremock canonicalizes the incoming path, so
        // this test would also catch a regression that sent "//".
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = OneCliClient::new(OneCliConfig {
            url: format!("{}/", server.uri()), // force trailing slash
            agent_id: "slash-test".into(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();

        let resp = client
            .get("https://api.example.com/anything")
            .send()
            .await
            .expect("request reached proxy");
        assert_eq!(resp.status(), 200);
    }

    /// The Debug impl must not include credential-like fields (it
    /// doesn't store any today, but this guards against a future
    /// regression if someone adds an `api_key: String` field to
    /// OneCliConfig and gets it for free via `#[derive(Debug)]`).
    /// Keeping this as a structural check on the fields we expect
    /// to see, and negative assertions on common credential words.
    #[test]
    fn debug_format_does_not_expose_common_credential_fields() {
        let config = OneCliConfig {
            url: "http://proxy:8081".to_string(),
            agent_id: "test-agent".to_string(),
            timeout: Duration::from_secs(10),
        };
        let client = OneCliClient::new(config).unwrap();
        let debug_str = format!("{:?}", client);
        // Positive: fields we intend to be debug-visible.
        assert!(debug_str.contains("http://proxy:8081"));
        assert!(debug_str.contains("test-agent"));
        // Negative: these tokens must never appear. If someone adds a
        // credential-bearing field and Debug picks it up, this fails.
        for forbidden in ["api_key", "secret", "token", "password", "Bearer"] {
            assert!(
                !debug_str.contains(forbidden),
                "Debug output unexpectedly contains {forbidden:?}: {debug_str}"
            );
        }
    }

    /// Construction with a valid config must yield a usable client that
    /// can actually hit a server. This consolidates the two prior
    /// "is_ok" tests into one behavioral test — previously they only
    /// asserted the builder's infallible path.
    #[tokio::test]
    async fn newly_built_client_can_send_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = OneCliClient::new(OneCliConfig {
            url: server.uri(),
            agent_id: "liveness".into(),
            timeout: Duration::from_secs(5),
        })
        .expect("client builds");

        let resp = client
            .get("https://api.example.com/any")
            .send()
            .await
            .expect("client can send");
        assert_eq!(resp.status(), 200);
    }
}
