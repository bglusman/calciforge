// Env mutation is serialized via ENV_MUTEX; holding a std Mutex across
// `.await` is therefore safe here (each #[tokio::test] has its own
// single-threaded runtime, no re-entrant lock attempts). Opt out of
// the lint narrowly rather than using tokio::sync::Mutex which isn't
// const-constructible for a static.
#![allow(clippy::await_holding_lock)]

//! Behavioral tests for the `/vault/:secret` route migrated from
//! onecli-client per #28.
//!
//! Approach: spin up the real axum router (same one main.rs builds via
//! `security_proxy::build_app`) in an in-process task and hit it via
//! reqwest. No separate process, no drift risk between test wiring and
//! production wiring — both call the same builder.
//!
//! Each test sets env vars to control the resolver output (via the
//! `<NAME>_API_KEY` convention). ENV_MUTEX serializes since env mutation
//! is process-global.

use std::sync::Arc;
use std::sync::Mutex;

use adversary_detector::{RateLimitConfig, ScannerConfig};
use security_proxy::build_app;
use security_proxy::config::GatewayConfig;
use security_proxy::proxy::SecurityProxy;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn set_env<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    // Safety: callers hold ENV_MUTEX; env mutation is serialized across
    // tests in this file.
    unsafe { std::env::set_var(key, value) }
}

fn remove_env<K: AsRef<std::ffi::OsStr>>(key: K) {
    unsafe { std::env::remove_var(key) }
}

async fn spawn_server() -> String {
    let proxy = SecurityProxy::new(
        GatewayConfig::default(),
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
    // Give the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Given an env var `TVROUTE_T1_API_KEY=hello` set,
/// when a client hits `GET /vault/tvroute_t1`,
/// then the response is 200 with body `{"status":"ok","secret":"tvroute_t1","token":"hello"}`.
///
/// Catches wiring regressions: route not registered, handler not
/// returning JSON, resolver not consulted, or the name-transform
/// convention drifting.
#[tokio::test]
async fn vault_route_returns_resolved_secret() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    set_env("TVROUTE_T1_API_KEY", "hello");

    let base = spawn_server().await;
    let resp = reqwest::get(format!("{base}/vault/tvroute_t1"))
        .await
        .expect("request should succeed");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("body is JSON");

    remove_env("TVROUTE_T1_API_KEY");

    assert_eq!(status, 200, "expected 200, got {status}");
    assert_eq!(body["status"], "ok", "status should be ok, got {body}");
    assert_eq!(body["secret"], "tvroute_t1");
    assert_eq!(body["token"], "hello");
}

/// Given no env var, no fnox entry, no vault token for the name,
/// when a client hits `GET /vault/tvroute_missing`,
/// then the response is 404 with a bland "Secret not found" message
/// (and crucially, does NOT leak the underlying error text — which
/// could include the env var name probed or the vault URL).
#[tokio::test]
async fn vault_route_returns_404_for_missing_secret_with_bland_message() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    remove_env("TVROUTE_MISSING_API_KEY");
    remove_env("ONECLI_VAULT_TOKEN");
    remove_env("ONECLI_VAULT_URL");

    let base = spawn_server().await;
    let resp = reqwest::get(format!("{base}/vault/tvroute_missing"))
        .await
        .expect("request should succeed");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("body is JSON");

    assert_eq!(status, 404, "missing secret must be 404, got {status}");
    assert_eq!(body["status"], "error");
    assert_eq!(
        body["message"], "Secret not found",
        "response body must use the bland message, not `e.to_string()` — \
         leaking the resolver's error text reveals which env vars/vault \
         endpoints we probed"
    );
    assert!(
        body.get("token").is_none(),
        "error response must not include a token field"
    );
}
