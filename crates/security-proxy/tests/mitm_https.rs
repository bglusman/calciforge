// Env mutation is serialized via ENV_MUTEX; the lock is held across awaits so
// the resolver cannot observe partially-restored process env.
#![allow(clippy::await_holding_lock)]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use adversary_detector::{RateLimitConfig, ScannerConfig};
use http_body_util::BodyExt;
use hudsucker::certificate_authority::{CertificateAuthority, RcgenAuthority};
use hudsucker::hyper::body::Incoming;
use hudsucker::hyper::service::service_fn;
use hudsucker::hyper::{header, Method, Request, Response, StatusCode};
use hudsucker::hyper_util::client::legacy::connect::HttpConnector;
use hudsucker::hyper_util::rt::{TokioExecutor, TokioIo};
use hudsucker::hyper_util::server::conn::auto;
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType, IsCa, KeyPair,
};
use hudsucker::rustls::{self, RootCertStore};
use reqwest::tls::Certificate;
use security_proxy::config::GatewayConfig;
use security_proxy::mitm::{install_default_crypto_provider, CalciforgeMitmHandler};
use security_proxy::proxy::SecurityProxy;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

static ENV_MUTEX: Mutex<()> = Mutex::new(());
type SeenRequestSender = Arc<Mutex<Option<oneshot::Sender<(String, String, String)>>>>;

fn set_env<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    unsafe { std::env::set_var(key, value) }
}

fn remove_env<K: AsRef<std::ffi::OsStr>>(key: K) {
    unsafe { std::env::remove_var(key) }
}

fn make_test_ca() -> (String, String) {
    install_default_crypto_provider();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Calciforge MITM Test CA");
    params.distinguished_name = dn;

    let signing_key = KeyPair::generate().expect("generate CA key");
    let issuer = CertifiedIssuer::self_signed(params, signing_key).expect("self-signed CA");
    (issuer.pem(), issuer.key().serialize_pem())
}

fn rcgen_authority(ca_cert: &str, ca_key: &str) -> RcgenAuthority {
    let key_pair = KeyPair::from_pem(ca_key).expect("parse CA key");
    let issuer =
        hudsucker::rcgen::Issuer::from_ca_cert_pem(ca_cert, key_pair).expect("parse CA cert");
    RcgenAuthority::new(
        issuer,
        1000,
        hudsucker::rustls::crypto::aws_lc_rs::default_provider(),
    )
}

fn trusted_hyper_connector(ca_cert: &str) -> hyper_rustls::HttpsConnector<HttpConnector> {
    let mut roots = RootCertStore::empty();
    let mut cert_bytes = ca_cert.as_bytes();
    let cert = rustls_pemfile::certs(&mut cert_bytes)
        .next()
        .expect("one CA cert")
        .expect("parse CA cert");
    roots.add(cert).expect("add CA root");

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .build()
}

async fn start_https_upstream(
    ca: RcgenAuthority,
) -> (
    String,
    oneshot::Receiver<(String, String, String)>,
    oneshot::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor: tokio_rustls::TlsAcceptor = ca
        .gen_server_config(&"localhost".parse().unwrap())
        .await
        .into();
    let (seen_tx, seen_rx) = oneshot::channel();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let seen_tx = Arc::new(Mutex::new(Some(seen_tx)));

    tokio::spawn(async move {
        let server = auto::Builder::new(TokioExecutor::new());
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let (tcp, _) = accepted.unwrap();
                    let tls = acceptor.accept(tcp).await.unwrap();
                    let server = server.clone();
                    let tx = Arc::clone(&seen_tx);
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| {
                            let tx = Arc::clone(&tx);
                            test_service(req, tx)
                        });
                        server
                            .serve_connection_with_upgrades(TokioIo::new(tls), svc)
                            .await
                            .unwrap();
                    });
                }
            }
        }
    });

    (
        format!("https://localhost:{}", addr.port()),
        seen_rx,
        stop_tx,
    )
}

async fn test_service(
    req: Request<Incoming>,
    seen_tx: SeenRequestSender,
) -> Result<Response<hudsucker::Body>, Infallible> {
    if req.method() != Method::POST || req.uri().path() != "/secret" {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(hudsucker::Body::empty())
            .unwrap());
    }
    let header = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let proxy_auth = req
        .headers()
        .get(header::PROXY_AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = req.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let tx = seen_tx.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(tx) = tx {
        let _ = tx.send((header, proxy_auth, body));
    }
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(hudsucker::Body::empty())
        .unwrap())
}

async fn start_mitm_proxy(ca_cert: &str, ca_key: &str) -> String {
    start_mitm_proxy_with_config(
        ca_cert,
        ca_key,
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            ..GatewayConfig::default()
        },
    )
    .await
}

async fn start_mitm_proxy_with_config(
    ca_cert: &str,
    ca_key: &str,
    config: GatewayConfig,
) -> String {
    let proxy =
        SecurityProxy::new(config, ScannerConfig::default(), RateLimitConfig::default()).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ca = rcgen_authority(ca_cert, ca_key);
    let state = Arc::new(proxy);
    let handler = CalciforgeMitmHandler::new(Arc::clone(&state));
    let proxy = hudsucker::Proxy::builder()
        .with_listener(listener)
        .with_ca(ca)
        .with_http_connector(trusted_hyper_connector(ca_cert))
        .with_http_handler(handler)
        .with_graceful_shutdown(std::future::pending())
        .build()
        .expect("build MITM proxy");
    tokio::spawn(proxy.start());
    format!("http://{addr}")
}

#[tokio::test]
async fn https_mitm_substitutes_header_and_json_body_before_forwarding() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("SECURITY_PROXY_VAULT_TOKEN", "mitm-vault-token");
    set_env("MITM_TEST_API_KEY", "super-secret");

    let (ca_cert, ca_key) = make_test_ca();
    let (upstream, seen_rx, stop_upstream) =
        start_https_upstream(rcgen_authority(&ca_cert, &ca_key)).await;
    let proxy = start_mitm_proxy(&ca_cert, &ca_key).await;

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .add_root_certificate(Certificate::from_pem(ca_cert.as_bytes()).unwrap())
        .no_brotli()
        .no_deflate()
        .no_gzip()
        .build()
        .unwrap();

    let resp = client
        .post(format!("{upstream}/secret"))
        .header("Content-Type", "application/json")
        .header("X-Api-Key", "{{secret:mitm_test}}")
        .header("Proxy-Authorization", "Basic should-not-forward")
        .body(r#"{"key":"{{secret:mitm_test}}","other":"literal"}"#)
        .send()
        .await
        .expect("HTTPS request succeeds through MITM proxy");

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let (header, proxy_auth, body) = seen_rx.await.expect("upstream observed request");
    assert_eq!(header, "super-secret");
    assert_eq!(proxy_auth, "");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"key": "super-secret", "other": "literal"})
    );

    let vault_resp = reqwest::Client::new()
        .get(format!("{proxy}/vault/mitm_test"))
        .bearer_auth("mitm-vault-token")
        .send()
        .await
        .expect("MITM proxy still serves local control routes");
    assert_eq!(vault_resp.status(), StatusCode::OK);
    assert_eq!(
        vault_resp.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({
            "status": "ok",
            "secret": "mitm_test",
            "token": "super-secret"
        })
    );

    let _ = stop_upstream.send(());
    remove_env("MITM_TEST_API_KEY");
    remove_env("SECURITY_PROXY_VAULT_TOKEN");
}

/// Given an allowlist that ONLY allows `api.anthropic.com`,
/// when the agent sends a header `{{secret:mitm_locked}}` to a different
/// host (the wiremock-style upstream),
/// then substitution is REFUSED and upstream is never hit.
///
/// This is the headline §11.1 attack: a prompt-injected agent
/// constructing a request to attacker-controlled host. Without this
/// check, the gateway would substitute and exfiltrate. Equivalent to
/// the deleted axum-mode test in destination_allowlist.rs.
#[tokio::test]
async fn https_mitm_destination_allowlist_blocks_disallowed_host() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    set_env("MITM_LOCKED_API_KEY", "should-never-leave-the-process");

    let (ca_cert, ca_key) = make_test_ca();
    let (upstream, _seen_rx, stop_upstream) =
        start_https_upstream(rcgen_authority(&ca_cert, &ca_key)).await;

    let mut allow = std::collections::HashMap::new();
    allow.insert("mitm_locked".into(), vec!["api.anthropic.com".into()]);
    let proxy = start_mitm_proxy_with_config(
        &ca_cert,
        &ca_key,
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            secret_destination_allowlist: allow,
            ..GatewayConfig::default()
        },
    )
    .await;

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .add_root_certificate(Certificate::from_pem(ca_cert.as_bytes()).unwrap())
        .no_brotli()
        .no_deflate()
        .no_gzip()
        .build()
        .unwrap();
    let resp = client
        .post(format!("{upstream}/secret"))
        .header("Content-Type", "application/json")
        .header("X-Api-Key", "{{secret:mitm_locked}}")
        .body("{}")
        .send()
        .await
        .expect("gateway returns SOME response");

    // Block responses are 200/HTML by design (see mitm_blocked_response);
    // X-Calciforge-Blocked is the structured signal.
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("X-Calciforge-Blocked")
            .and_then(|v| v.to_str().ok()),
        Some("true"),
    );
    let body = resp.text().await.unwrap_or_default();
    assert!(
        !body.contains("should-never-leave-the-process"),
        "body must not echo the secret value"
    );
    assert!(
        !body.contains("mitm_locked"),
        "body must not echo the secret name"
    );

    let _ = stop_upstream.send(());
    remove_env("MITM_LOCKED_API_KEY");
}

/// Given a request body with content-type `application/octet-stream`
/// containing `{{secret:NAME}}` as ASCII bytes,
/// when the client forwards through the MITM gateway,
/// then the gateway serves a block page rather than forwarding.
/// Covers RFC §11.8 — an agent can't hide a ref in an unsupported
/// content-type to skip substitution. Equivalent to the deleted
/// axum-mode test in substitution_body_headers.rs.
#[tokio::test]
async fn https_mitm_blocks_ref_in_unsupported_content_type() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECRETS_VAULT_TOKEN");
    remove_env("SECRETS_VAULT_URL");
    remove_env("MITM_RAW_API_KEY");

    let (ca_cert, ca_key) = make_test_ca();
    let (upstream, _seen_rx, stop_upstream) =
        start_https_upstream(rcgen_authority(&ca_cert, &ca_key)).await;
    let proxy = start_mitm_proxy(&ca_cert, &ca_key).await;

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .add_root_certificate(Certificate::from_pem(ca_cert.as_bytes()).unwrap())
        .no_brotli()
        .no_deflate()
        .no_gzip()
        .build()
        .unwrap();
    let resp = client
        .post(format!("{upstream}/secret"))
        .header("Content-Type", "application/octet-stream")
        .body(b"prefix {{secret:mitm_raw}} suffix".to_vec())
        .send()
        .await
        .expect("gateway returns SOME response");

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("X-Calciforge-Blocked")
            .and_then(|v| v.to_str().ok()),
        Some("true"),
    );

    let _ = stop_upstream.send(());
}

/// Given the vault bearer token env var is unset,
/// when a client hits `GET /vault/anything` on the MITM proxy port,
/// then the response is 503 (route disabled / not an oracle).
/// Equivalent to the deleted axum-mode test in vault_route.rs.
#[tokio::test]
async fn https_mitm_vault_route_503_when_token_env_unset() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    remove_env("SECURITY_PROXY_VAULT_TOKEN");

    let (ca_cert, ca_key) = make_test_ca();
    let proxy = start_mitm_proxy(&ca_cert, &ca_key).await;

    let resp = reqwest::get(format!("{proxy}/vault/anything"))
        .await
        .expect("control-plane request succeeds");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "vault route must be disabled when bearer token env unset"
    );
}

/// Given the vault bearer token IS set but the request omits/wrongs
/// the Bearer header,
/// when a client hits `GET /vault/anything` on the MITM proxy port,
/// then the response is 401. Equivalent to the deleted axum-mode test
/// in vault_route.rs.
#[tokio::test]
async fn https_mitm_vault_route_401_when_bearer_missing_or_wrong() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    set_env("SECURITY_PROXY_VAULT_TOKEN", "expected-vault-token");

    let (ca_cert, ca_key) = make_test_ca();
    let proxy = start_mitm_proxy(&ca_cert, &ca_key).await;

    let resp_no_header = reqwest::get(format!("{proxy}/vault/anything"))
        .await
        .expect("control-plane request succeeds");
    assert_eq!(resp_no_header.status(), StatusCode::UNAUTHORIZED);

    let resp_wrong = reqwest::Client::new()
        .get(format!("{proxy}/vault/anything"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .expect("control-plane request succeeds");
    assert_eq!(resp_wrong.status(), StatusCode::UNAUTHORIZED);

    remove_env("SECURITY_PROXY_VAULT_TOKEN");
}
