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
    let proxy = SecurityProxy::new(
        GatewayConfig {
            scan_outbound: false,
            scan_inbound: false,
            bypass_domains: vec![],
            ..GatewayConfig::default()
        },
        ScannerConfig::default(),
        RateLimitConfig::default(),
    )
    .await;

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
