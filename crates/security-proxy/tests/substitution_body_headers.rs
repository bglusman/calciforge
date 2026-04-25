// Env mutation is serialized via ENV_MUTEX; holding a std Mutex across
// `.await` is narrow here and doesn't spawn inner tasks that re-acquire
// the lock. See vault_route.rs for the same pattern and its rationale.
#![allow(clippy::await_holding_lock)]

//! Behavioral tests for `{{secret:NAME}}` substitution in request
//! HEADERS and BODY — extending the URL-path tests that live in the
//! substitution engine's inline suite.
//!
//! Approach: spin up the real intercept pipeline in-process (via
//! `security_proxy::build_app`), point the test harness at a
//! wiremock-backed upstream so we can observe what the gateway
//! actually forwards, and run one request per behavioral case.
//!
//! Each test sets an env var for the resolver to find, then asserts
//! on what the mock upstream received — never on our own code's
//! return value. Catches wiring regressions end-to-end.

use std::sync::Arc;
use std::sync::Mutex;

use adversary_detector::{RateLimitConfig, ScannerConfig};
use security_proxy::build_app;
use security_proxy::config::GatewayConfig;
use security_proxy::proxy::SecurityProxy;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn set_env<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    unsafe { std::env::set_var(key, value) }
}

fn remove_env<K: AsRef<std::ffi::OsStr>>(key: K) {
    unsafe { std::env::remove_var(key) }
}

async fn spawn_gateway() -> (String, MockServer) {
    let upstream = MockServer::start().await;

    let proxy = SecurityProxy::new(
        GatewayConfig {
            // Keep outbound scanning off so our body payloads aren't
            // treated as exfil signals — this suite is about
            // substitution wiring, not scanner behavior.
            scan_outbound: false,
            scan_inbound: false,
            // Don't bypass loopback — the upstream runs on 127.0.0.1.
            bypass_domains: vec![],
            // Inject_credentials is irrelevant here; the substitution
            // path doesn't depend on it.
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

    // Poll /health until ready.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let base = format!("http://{addr}");
    while std::time::Instant::now() < deadline {
        if client
            .get(format!("{base}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return (base, upstream);
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("spawn_gateway: health check did not become ready within 2s");
}

/// Given an env var `TSB_H1_API_KEY=hello`,
/// when a client sends a request with header `X-Api-Key: {{secret:tsb_h1}}`,
/// then the upstream receives `X-Api-Key: hello` (substituted).
///
/// Catches header-value wiring regressions: substitution must run on
/// header values, not just the URL. A broken wiring would forward the
/// literal `{{secret:…}}` string to upstream — upstream sees it, logs
/// it, now the ref name leaks.
#[tokio::test]
async fn header_value_with_ref_is_substituted_before_forward() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("ONECLI_VAULT_TOKEN");
    remove_env("ONECLI_VAULT_URL");
    set_env("TSB_H1_API_KEY", "hello");

    let (gateway, upstream) = spawn_gateway().await;

    Mock::given(method("GET"))
        .and(path("/ping"))
        .and(header("x-api-key", "hello"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&upstream)
        .await;

    // The intercept handler reconstructs the target URL from the
    // incoming URI's scheme — we must send with an absolute URI to
    // reach upstream (proxy/CONNECT-style). reqwest's `get(abs_url)`
    // hits the gateway at its base, while passing the absolute-form
    // URI through requires a proxy config. Simplest pattern: send
    // directly to the gateway path `{base}/ping` and let the gateway
    // treat that as the target. Intercept reconstructs accordingly.
    //
    // For this test, we send a request whose full URL points at
    // upstream.uri() but the reqwest client uses the gateway as a
    // proxy so the request-line has the absolute URI form.
    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/ping", upstream.uri()))
        .header("X-Api-Key", "{{secret:tsb_h1}}")
        .send()
        .await
        .expect("request reached upstream via gateway");
    assert_eq!(resp.status(), 204);

    remove_env("TSB_H1_API_KEY");
    // wiremock's .expect(1) + drop-time assertion confirms the
    // substituted header arrived exactly once.
}

/// Given an env var `TSB_B1_API_KEY=somevalue`,
/// when a client POSTs a JSON body containing `{"key":"{{secret:tsb_b1}}"}`,
/// then the upstream receives the body with `somevalue` substituted
/// in place of the ref.
#[tokio::test]
async fn json_body_with_ref_is_substituted_before_forward() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // Clear vault env to avoid a real network roundtrip when the
    // resolver's env check hits; the env var below is the one we
    // want to resolve.
    remove_env("ONECLI_VAULT_TOKEN");
    remove_env("ONECLI_VAULT_URL");
    set_env("TSB_B1_API_KEY", "somevalue");

    let (gateway, upstream) = spawn_gateway().await;

    Mock::given(method("POST"))
        .and(path("/data"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "key": "somevalue",
            "other": "literal",
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .post(format!("{}/data", upstream.uri()))
        .header("Content-Type", "application/json")
        .body(r#"{"key":"{{secret:tsb_b1}}","other":"literal"}"#)
        .send()
        .await
        .expect("request reached upstream via gateway");
    assert_eq!(resp.status(), 200);

    remove_env("TSB_B1_API_KEY");
}

/// Given a request body with content-type `application/octet-stream`
/// containing `{{secret:tsb_raw1}}` as ASCII bytes,
/// when the client forwards through the gateway,
/// then the gateway rejects the request with 403 rather than
/// forwarding. Covers the RFC §11.8 smuggling case: an agent can't
/// hide a ref in an unsupported content-type to skip substitution.
#[tokio::test]
async fn ref_in_unsupported_content_type_body_is_blocked() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // No env var for TSB_RAW1 — doesn't matter, the scan runs before
    // resolution.
    remove_env("TSB_RAW1_API_KEY");

    let (gateway, upstream) = spawn_gateway().await;

    // Mount a handler that should NEVER be hit. If it is, the test
    // fails because wiremock's `.expect(0)` + drop checks zero calls.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .post(format!("{}/binary", upstream.uri()))
        .header("Content-Type", "application/octet-stream")
        .body(b"some prefix {{secret:tsb_raw1}} some suffix".to_vec())
        .send()
        .await
        .expect("gateway returns SOME response, even if blocked");
    assert_eq!(
        resp.status(),
        403,
        "unsupported content-type with ref must be blocked; got {}",
        resp.status()
    );
}

/// Given a body containing an unresolvable ref in a JSON body
/// (content-type supports substitution, but the ref doesn't resolve),
/// when the client forwards through the gateway,
/// then the gateway rejects with 403 and upstream never sees the request.
/// Same pattern as the URL test — caller mustn't be able to exfil the
/// ref name to upstream by forcing a failure.
#[tokio::test]
async fn unresolvable_ref_in_body_blocks_request() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("TSB_MISSING_API_KEY");
    remove_env("ONECLI_VAULT_TOKEN");
    remove_env("ONECLI_VAULT_URL");

    let (gateway, upstream) = spawn_gateway().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .post(format!("{}/data", upstream.uri()))
        .header("Content-Type", "application/json")
        .body(r#"{"k":"{{secret:tsb_missing}}"}"#)
        .send()
        .await
        .expect("gateway responds");
    assert_eq!(resp.status(), 403);
    // And crucially, the error body must not echo the ref name.
    let body = resp.text().await.unwrap_or_default();
    assert!(
        !body.contains("tsb_missing"),
        "error body must not leak the ref name; got: {body}"
    );
}
