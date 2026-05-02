//! Integration tests for `[security.agent_web]` policy hooks.
//!
//! These exercise the four labeled features end-to-end via the
//! axum-router intercept path (same path the MITM handler shares the
//! policy logic with). For each labeled feature there's at least one
//! "block / strip" test and one "pass-through when disabled" test.

use std::sync::Arc;

use adversary_detector::{RateLimitConfig, ScannerConfig};
use security_proxy::build_app;
use security_proxy::config::{AgentWebPolicy, GatewayConfig};
use security_proxy::proxy::SecurityProxy;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn spawn_gateway_with(policy: AgentWebPolicy) -> (String, MockServer) {
    let upstream = MockServer::start().await;
    let proxy = SecurityProxy::new(
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            agent_web: policy,
            ..GatewayConfig::default()
        },
        ScannerConfig::default(),
        RateLimitConfig::default(),
    )
    .await;
    let app = build_app(Arc::new(proxy));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    // Wait for /health
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if client
            .get(format!("{base}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    (base, upstream)
}

// The axum proxy_handler intercepts ANY URL the client sends, so we can
// rewrite `target_url` directly to the wiremock server. To force the
// proxy to see "host = api.search.brave.com" we instead send the
// request with `Host:` header equal to the test value but we can't make
// reqwest::Url::parse return that — so we use the technique from
// destination_allowlist.rs: send the upstream URL as the request URI,
// and assert behavior based on the mock receiving (or not) the request.
//
// To exercise host-pattern matching we mount the wiremock at 127.0.0.1
// and configure the agent_web `search_engine_patterns` to include
// "127.0.0.1" — same matching code path, fully behavioral.

// ─── (A) Search-engine egress block ───────────────────────────────────

#[tokio::test]
async fn search_engine_egress_blocked_when_forbidden() {
    let policy = AgentWebPolicy {
        forbid_search_engines: true,
        search_engine_patterns: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/proxy"))
        .header("host", "api.search.brave.com")
        .query(&[("__test_target", format!("{}/search", upstream.uri()))])
        // Use the host directly: the gateway parses the URL we send.
        .send()
        .await
        .unwrap();

    // The request hits the gateway with target = wiremock localhost.
    // We instead use a plain forward via "GET <upstream>/search" and the
    // dest_host = 127.0.0.1, which our pattern matches.
    let _ = resp;

    let resp = client
        .get(format!("{}/search", upstream.uri()))
        .header(
            "x-forwarded-via-proxy",
            "ignored-but-shows-this-is-direct-pass-through-test",
        )
        .send()
        .await
        .unwrap();
    // Sanity: direct call works.
    assert_eq!(resp.status(), 200);

    // Now use the proxy: configure reqwest to use it.
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/search", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "search-engine host must be blocked");
}

#[tokio::test]
async fn search_engine_egress_allowed_when_disabled() {
    let policy = AgentWebPolicy {
        forbid_search_engines: false,
        search_engine_patterns: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/search", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

// ─── (B) Search-response scanning ─────────────────────────────────────

#[tokio::test]
async fn search_response_blocked_when_contains_denied_url() {
    let policy = AgentWebPolicy {
        forbid_search_engines: false,
        scan_search_responses: true,
        search_engine_patterns: vec!["127.0.0.1".into()],
        url_destination_denylist: vec!["blocked.example.com".into()],
        search_response_strategy: "block".into(),
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"web":{"results":[{"url":"https://blocked.example.com/x","title":"t"}]}}"#,
        ))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/search", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "denied URL in response must block");
}

#[tokio::test]
async fn search_response_strip_drops_offending_entries() {
    let policy = AgentWebPolicy {
        forbid_search_engines: false,
        scan_search_responses: true,
        search_engine_patterns: vec!["127.0.0.1".into()],
        url_destination_denylist: vec!["blocked.example.com".into()],
        search_response_strategy: "strip".into(),
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"web":{"results":[{"url":"https://blocked.example.com/x","title":"bad"},{"url":"https://allowed.com/y","title":"good"}]}}"#,
        ))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/search", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(!text.contains("blocked.example.com"), "stripped: {text}");
    assert!(text.contains("allowed.com"), "kept the good one: {text}");
}

#[tokio::test]
async fn search_response_passes_when_no_denylist_hit() {
    let policy = AgentWebPolicy {
        forbid_search_engines: false,
        scan_search_responses: true,
        search_engine_patterns: vec!["127.0.0.1".into()],
        url_destination_denylist: vec!["blocked.example.com".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"web":{"results":[{"url":"https://allowed.com"}]}}"#),
        )
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/search", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

// ─── (C) Provider-browsing strip / block ──────────────────────────────

#[tokio::test]
async fn provider_browsing_strips_openai_web_search_tool() {
    let policy = AgentWebPolicy {
        forbid_provider_browsing: true,
        provider_browsing_strategy: "strip".into(),
        // Treat the wiremock loopback as the LLM API for the test.
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "gpt-4o",
        "tools": [
            {"type": "web_search", "name": "web_search"},
            {"type": "function", "name": "calc"}
        ]
    });
    let r = proxied
        .post(format!("{}/v1/chat/completions", upstream.uri()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Verify upstream received the modified body.
    let received = upstream.received_requests().await.unwrap();
    let last = received.last().unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
    let tools = parsed["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "calc");
}

#[tokio::test]
async fn provider_browsing_blocks_anthropic_web_search_when_strategy_block() {
    let policy = AgentWebPolicy {
        forbid_provider_browsing: true,
        provider_browsing_strategy: "block".into(),
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, _upstream) = spawn_gateway_with(policy).await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "claude-3-5-sonnet",
        "tools": [{"type": "web_search_20250305", "name": "web_search_20250305"}]
    });
    let r = proxied
        .post("http://127.0.0.1:1/v1/messages")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn provider_browsing_always_blocks_search_model() {
    let policy = AgentWebPolicy {
        forbid_provider_browsing: true,
        provider_browsing_strategy: "strip".into(),
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, _upstream) = spawn_gateway_with(policy).await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({"model": "gpt-4o-search-preview"});
    let r = proxied
        .post("http://127.0.0.1:1/v1/chat/completions")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "search-preview model must always block");
}

// ─── (D) URL pre-flight ───────────────────────────────────────────────

#[tokio::test]
async fn preflight_blocks_url_in_user_message_string() {
    let policy = AgentWebPolicy {
        preflight_message_urls: true,
        url_destination_denylist: vec!["blocked.example.com".into()],
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, _upstream) = spawn_gateway_with(policy).await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "summarize https://blocked.example.com/x"}]
    });
    let r = proxied
        .post("http://127.0.0.1:1/v1/chat/completions")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn preflight_blocks_url_in_anthropic_content_array() {
    let policy = AgentWebPolicy {
        preflight_message_urls: true,
        url_destination_denylist: vec!["blocked.example.com".into()],
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, _upstream) = spawn_gateway_with(policy).await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "claude-3-5-sonnet",
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "see https://blocked.example.com/x"}
        ]}]
    });
    let r = proxied
        .post("http://127.0.0.1:1/v1/messages")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn preflight_passes_when_disabled() {
    let policy = AgentWebPolicy {
        preflight_message_urls: false,
        url_destination_denylist: vec!["blocked.example.com".into()],
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "summarize https://blocked.example.com/x"}]
    });
    let r = proxied
        .post(format!("{}/v1/chat/completions", upstream.uri()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn preflight_passes_clean_message() {
    let policy = AgentWebPolicy {
        preflight_message_urls: true,
        url_destination_denylist: vec!["blocked.example.com".into()],
        known_llm_apis: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "summarize https://allowed.com/x"}]
    });
    let r = proxied
        .post(format!("{}/v1/chat/completions", upstream.uri()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

// ─── Audit-log smoke test (B) ─────────────────────────────────────────
//
// We don't assert on the tracing output structure here — that's
// integration-fragile across versions; we just confirm the policy hit
// path doesn't panic and produces a 403 (the "decision" is in the trace).
#[tokio::test]
async fn audit_smoke_search_engines_blocked() {
    let policy = AgentWebPolicy {
        forbid_search_engines: true,
        search_engine_patterns: vec!["127.0.0.1".into()],
        ..AgentWebPolicy::default()
    };
    let (base, upstream) = spawn_gateway_with(policy).await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&upstream)
        .await;
    let proxied = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&base).unwrap())
        .build()
        .unwrap();
    let r = proxied
        .get(format!("{}/x", upstream.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Read body so we exercise the block-response writer.
    let _ = r.text().await.unwrap();
}
