//! HTTPS MITM proxy mode built on hudsucker.
//!
//! The existing Axum router remains the default explicit/plain HTTP proxy and
//! control-plane path. This module is the HTTPS interception path: clients trust
//! the configured Calciforge CA, send `HTTP_PROXY`/`HTTPS_PROXY` traffic here,
//! and hudsucker gives Calciforge decrypted HTTP requests/responses to scan and
//! rewrite before forwarding upstream.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;

use adversary_detector::ScanContext;
use anyhow::{anyhow, Context, Result};
use http_body_util::{BodyExt, Full};
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::hyper::body::Bytes;
use hudsucker::hyper::header;
use hudsucker::hyper::{Method, Request, Response, StatusCode};
use hudsucker::rcgen::{Issuer, KeyPair};
use hudsucker::rustls::crypto::aws_lc_rs;
use hudsucker::{Body as MitmBody, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::proxy::{self, BodyMode, SecurityProxy};
use crate::router;

static CRYPTO_PROVIDER_INIT: Once = Once::new();

/// Install a process-wide rustls crypto provider. Pulling hudsucker in enables
/// aws-lc-rs while this crate also used rustls directly, so rustls can no
/// longer infer a single provider automatically.
pub fn install_default_crypto_provider() {
    CRYPTO_PROVIDER_INIT.call_once(|| {
        let _ = aws_lc_rs::default_provider().install_default();
    });
}

/// Load a PEM CA pair into the certificate authority hudsucker uses to mint
/// per-origin certificates during CONNECT interception.
pub fn load_rcgen_authority(cert_path: &str, key_path: &str) -> Result<RcgenAuthority> {
    install_default_crypto_provider();
    let ca_cert = std::fs::read_to_string(cert_path)
        .with_context(|| format!("read MITM CA certificate from {cert_path}"))?;
    let ca_key = std::fs::read_to_string(key_path)
        .with_context(|| format!("read MITM CA private key from {key_path}"))?;
    let key_pair = KeyPair::from_pem(&ca_key).context("parse MITM CA private key")?;
    let issuer =
        Issuer::from_ca_cert_pem(&ca_cert, key_pair).context("parse MITM CA certificate")?;
    Ok(RcgenAuthority::new(
        issuer,
        10_000,
        aws_lc_rs::default_provider(),
    ))
}

/// Start hudsucker MITM mode on an already-bound listener. The listener form is
/// useful for tests because callers can bind `127.0.0.1:0`, learn the chosen
/// port, and then start the proxy.
pub fn build_mitm_proxy(
    listener: TcpListener,
    state: Arc<SecurityProxy>,
    ca: RcgenAuthority,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<impl Future<Output = Result<(), hudsucker::Error>>> {
    install_default_crypto_provider();
    let handler = CalciforgeMitmHandler::new(state);
    let proxy = Proxy::builder()
        .with_listener(listener)
        .with_ca(ca)
        .with_rustls_connector(aws_lc_rs::default_provider())
        .with_http_handler(handler)
        .with_graceful_shutdown(shutdown)
        .build()
        .map_err(|err| anyhow!("build HTTPS MITM proxy: {err}"))?;
    Ok(proxy.start())
}

/// Start hudsucker MITM mode on `addr`.
pub async fn serve_mitm(
    addr: SocketAddr,
    state: Arc<SecurityProxy>,
    ca: RcgenAuthority,
) -> Result<()> {
    info!("Security proxy HTTPS MITM listening on {}", addr);
    let listener = TcpListener::bind(addr).await?;
    build_mitm_proxy(listener, state, ca, std::future::pending())?
        .await
        .map_err(|err| anyhow!("HTTPS MITM proxy stopped: {err}"))
}

#[derive(Clone)]
pub struct CalciforgeMitmHandler {
    state: Arc<SecurityProxy>,
    last_url: Option<String>,
}

impl CalciforgeMitmHandler {
    pub fn new(state: Arc<SecurityProxy>) -> Self {
        Self {
            state,
            last_url: None,
        }
    }

    fn health_response(&self) -> Response<MitmBody> {
        let audit_count = self.state.audit.count();
        let blocked = self.state.audit.blocked_and_reviewed();
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(MitmBody::from(format!(
                r#"{{"status":"ok","service":"security-gateway","mode":"https-mitm","version":"{}","total_requests":{},"blocked_or_reviewed":{}}}"#,
                env!("CARGO_PKG_VERSION"),
                audit_count,
                blocked
            )))
            .unwrap_or_else(|_| mitm_blocked_response("Failed to build response"))
    }

    async fn process_request(&mut self, req: Request<MitmBody>) -> RequestOrResponse {
        if req.method() == Method::CONNECT {
            return req.into();
        }
        if req.method() == Method::GET
            && req.uri().path() == "/health"
            && req.uri().scheme().is_none()
        {
            return RequestOrResponse::Response(self.health_response());
        }
        if req.method() == Method::GET
            && req.uri().path().starts_with("/vault/")
            && req.uri().scheme().is_none()
        {
            let secret_name = req.uri().path().trim_start_matches("/vault/").to_owned();
            return RequestOrResponse::Response(
                self.vault_response(req.headers(), secret_name).await,
            );
        }

        let req = match hudsucker::decode_request(req) {
            Ok(req) => req,
            Err(err) => {
                warn!("BLOCKED: failed to decode MITM request: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };

        let method = req.method().clone();
        let target_url = match request_target_url(&req) {
            Some(url) => url,
            None => {
                warn!("BLOCKED: MITM request target is not reconstructable");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };
        info!("MITM {} {}", method, target_url);

        let url_dest_host = reqwest::Url::parse(&target_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        if url_dest_host.is_none() && target_url.contains("{{secret:") {
            warn!("BLOCKED: MITM URL contains secret ref but host is unparseable");
            return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
        }

        let target_url = match self
            .state
            .resolve_and_substitute(&target_url, url_dest_host.as_deref())
            .await
        {
            Ok(url) => url,
            Err(err) => {
                warn!("BLOCKED: MITM URL substitution failed: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };
        self.last_url = Some(target_url.clone());

        let dest_host = reqwest::Url::parse(&target_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));

        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let (mut parts, body) = req.into_parts();
        parts.uri = match target_url.parse() {
            Ok(uri) => uri,
            Err(err) => {
                warn!("BLOCKED: substituted MITM URL is invalid: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };

        if let Err(err) =
            substitute_headers(&self.state, &mut parts.headers, dest_host.as_deref()).await
        {
            warn!("BLOCKED: MITM header substitution failed: {err}");
            return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
        }

        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                error!("Failed to read MITM request body: {err}");
                return RequestOrResponse::Response(mitm_blocked_response(
                    "Failed to read request body",
                ));
            }
        };
        let body_bytes = match substitute_body(
            &self.state,
            body_bytes,
            content_type.as_deref(),
            dest_host.as_deref(),
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(err) => {
                warn!("BLOCKED: MITM body substitution failed: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };

        if !self.state.check_bypassed(&target_url)
            && self.state.config.scan_outbound
            && !body_bytes.is_empty()
        {
            let body_text = String::from_utf8_lossy(&body_bytes);
            let verdict = self
                .state
                .scanner
                .scan(&target_url, &body_text, ScanContext::Api)
                .await;
            match verdict {
                adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                    warn!("BLOCKED MITM outbound to {}: {}", target_url, reason);
                    return RequestOrResponse::Response(mitm_blocked_response(&format!(
                        "Outbound request blocked: {reason}"
                    )));
                }
                adversary_detector::verdict::ScanVerdict::Review { reason } => {
                    info!("REVIEW MITM outbound to {}: {}", target_url, reason);
                }
                adversary_detector::verdict::ScanVerdict::Clean => {}
            }
        }

        if self.state.config.inject_credentials {
            if let Some(host) = dest_host.as_deref() {
                let mut injected_headers = Vec::new();
                if let Some(provider) = self.state.credentials.detect_provider_pub(host) {
                    let _ = self.state.credentials.ensure_cached(&provider).await;
                }
                self.state.credentials.inject(&mut injected_headers, host);
                for (name, value) in injected_headers {
                    if let (Ok(name), Ok(value)) = (
                        header::HeaderName::try_from(name.as_str()),
                        header::HeaderValue::try_from(value.as_str()),
                    ) {
                        parts.headers.insert(name, value);
                    }
                }
            }
        }

        remove_hop_by_hop_or_recomputed_headers(&mut parts.headers);
        Request::from_parts(parts, mitm_body_from_bytes(body_bytes)).into()
    }

    async fn process_response(&mut self, res: Response<MitmBody>) -> Response<MitmBody> {
        let res = match hudsucker::decode_response(res) {
            Ok(res) => res,
            Err(err) => {
                warn!("BLOCKED: failed to decode MITM response: {err}");
                return mitm_blocked_response("Response rejected");
            }
        };

        let target_url = self.last_url.as_deref().unwrap_or("<unknown>");
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();

        let (mut parts, body) = res.into_parts();
        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                error!("Failed to read MITM response body: {err}");
                return mitm_blocked_response("Failed to read response body");
            }
        };

        if self.state.config.scan_inbound && content_type.starts_with("text/") {
            if let Ok(body_str) = std::str::from_utf8(&body_bytes) {
                let verdict = self
                    .state
                    .scanner
                    .scan(target_url, body_str, ScanContext::WebFetch)
                    .await;
                match verdict {
                    adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                        warn!("BLOCKED MITM response from {}: {}", target_url, reason);
                        return mitm_blocked_response(&format!("Response blocked: {reason}"));
                    }
                    adversary_detector::verdict::ScanVerdict::Review { reason } => {
                        info!("REVIEW MITM response from {}: {}", target_url, reason);
                    }
                    adversary_detector::verdict::ScanVerdict::Clean => {}
                }
            }
        }

        remove_hop_by_hop_or_recomputed_headers(&mut parts.headers);
        Response::from_parts(parts, mitm_body_from_bytes(body_bytes))
    }

    async fn vault_response(
        &self,
        headers: &header::HeaderMap,
        secret_name: String,
    ) -> Response<MitmBody> {
        let (status, value) = router::vault_json_response(headers, secret_name).await;
        json_response(status, value)
    }
}

impl HttpHandler for CalciforgeMitmHandler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<MitmBody>,
    ) -> RequestOrResponse {
        self.process_request(req).await
    }

    async fn handle_response(
        &mut self,
        _ctx: &HttpContext,
        res: Response<MitmBody>,
    ) -> Response<MitmBody> {
        self.process_response(res).await
    }

    async fn should_intercept(&mut self, _ctx: &HttpContext, _req: &Request<MitmBody>) -> bool {
        true
    }
}

fn request_target_url(req: &Request<MitmBody>) -> Option<String> {
    if req.uri().scheme().is_some() {
        return Some(req.uri().to_string());
    }
    let host = req.headers().get(header::HOST)?.to_str().ok()?;
    Some(format!(
        "http://{}{}",
        host,
        req.uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
    ))
}

async fn substitute_headers(
    state: &SecurityProxy,
    headers: &mut header::HeaderMap,
    dest_host: Option<&str>,
) -> Result<(), String> {
    let original: Vec<(header::HeaderName, header::HeaderValue)> = headers
        .iter()
        .filter_map(|(name, value)| {
            if is_hop_by_hop_or_recomputed(name) {
                return None;
            }
            Some((name.clone(), value.clone()))
        })
        .collect();

    for (name, value) in original {
        let Ok(value_str) = value.to_str() else {
            continue;
        };
        let substituted = state.resolve_and_substitute(value_str, dest_host).await?;
        let header_value = header::HeaderValue::try_from(substituted.as_str())
            .map_err(|err| format!("invalid substituted header value for {name}: {err}"))?;
        headers.insert(name, header_value);
    }

    headers.remove(header::CONTENT_LENGTH);
    Ok(())
}

async fn substitute_body(
    state: &SecurityProxy,
    body_bytes: Bytes,
    content_type: Option<&str>,
    dest_host: Option<&str>,
) -> Result<Bytes, String> {
    if body_bytes.is_empty() {
        return Ok(body_bytes);
    }

    match SecurityProxy::body_substitution_mode(content_type) {
        BodyMode::FullSubstitute => {
            let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
            state
                .resolve_and_substitute(&body_str, dest_host)
                .await
                .map(|substituted| Bytes::from(substituted.into_bytes()))
        }
        BodyMode::RawScan => {
            if proxy::memchr_substr(&body_bytes, b"{{secret:") {
                return Err(format!(
                    "secret reference in body with unsupported content-type ({})",
                    content_type.unwrap_or("unset")
                ));
            }
            Ok(body_bytes)
        }
    }
}

fn is_hop_by_hop_or_recomputed(name: &header::HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn remove_hop_by_hop_or_recomputed_headers(headers: &mut header::HeaderMap) {
    for name in [
        header::HOST,
        header::CONNECTION,
        header::CONTENT_LENGTH,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        header::HeaderName::from_static("keep-alive"),
    ] {
        headers.remove(name);
    }
}

fn mitm_body_from_bytes(bytes: Bytes) -> MitmBody {
    MitmBody::from(Full::new(bytes))
}

fn mitm_blocked_response(reason: &str) -> Response<MitmBody> {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(MitmBody::from(format!(
            r#"{{"blocked":true,"reason":"{}"}}"#,
            reason.replace('"', "\\\"")
        )))
        .unwrap_or_else(|_| Response::new(MitmBody::from(r#"{"blocked":true}"#)))
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response<MitmBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(MitmBody::from(value.to_string()))
        .unwrap_or_else(|_| mitm_blocked_response("Failed to build response"))
}
