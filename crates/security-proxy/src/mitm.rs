//! HTTPS MITM proxy mode built on hudsucker — the *only* proxy mode in
//! the security-proxy binary as of 2026-04.
//!
//! Clients trust the configured Calciforge CA, send `HTTP_PROXY` /
//! `HTTPS_PROXY` traffic to this listener, and hudsucker hands Calciforge
//! decrypted HTTP requests/responses to scan and rewrite before forwarding
//! upstream. Plain-HTTP requests come through the same listener and use
//! the same pipeline; the local `/health` and `/vault/:secret` control
//! routes are also served from here so there's a single entry point.
//!
//! The legacy axum forward-proxy was deleted in this revision: it could
//! not inspect HTTPS (returned 400 to CONNECT) so in 2026 it functioned
//! as silent broken protection. One mode, one audit trail.

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

use crate::agent_web::{
    self, host_is_known_llm_api, host_matches_search_engine, BrowsingDecision,
    SearchResponseDecision,
};
use crate::proxy::{self, BodyMode, SecurityProxy};

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
    /// True when the last forwarded request was sent to a host matching
    /// `[security.agent_web].search_engine_patterns`. Used in
    /// `process_response` to apply (B) search-response scanning.
    last_was_search_host: bool,
}

impl CalciforgeMitmHandler {
    pub fn new(state: Arc<SecurityProxy>) -> Self {
        Self {
            state,
            last_url: None,
            last_was_search_host: false,
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
                return RequestOrResponse::Response(mitm_blocked_response(&format!(
                    "Failed to decode incoming request: {err}"
                )));
            }
        };

        let method = req.method().clone();
        let target_url = match request_target_url(&req) {
            Some(url) => url,
            None => {
                warn!("BLOCKED: MITM request target is not reconstructable");
                return RequestOrResponse::Response(mitm_blocked_response(
                    "Request URL could not be reconstructed; the gateway refuses requests \
                     it cannot identify a destination for.",
                ));
            }
        };
        info!("MITM {} {}", method, target_url);

        let url_dest_host = reqwest::Url::parse(&target_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        if url_dest_host.is_none() && target_url.contains("{{secret:") {
            warn!("BLOCKED: MITM URL contains secret ref but host is unparseable");
            return RequestOrResponse::Response(mitm_blocked_response(
                "URL contains a secret reference but the host portion could not be parsed; \
                 the gateway refuses to substitute secrets without a known destination.",
            ));
        }

        let target_url = match self
            .state
            .resolve_and_substitute(&target_url, url_dest_host.as_deref())
            .await
        {
            Ok(url) => url,
            Err(err) => {
                // Bland message; the err text contains the secret name.
                warn!("BLOCKED: MITM URL substitution failed: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };
        self.last_url = Some(target_url.clone());

        let dest_host = reqwest::Url::parse(&target_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));

        // (A) Search-engine egress block. Fires before body decoding so
        // we don't waste cycles on a request we're going to refuse.
        let policy = &self.state.config.agent_web;
        let host_is_search = match dest_host.as_deref() {
            Some(h) => host_matches_search_engine(h, &policy.search_engine_patterns),
            None => false,
        };
        self.last_was_search_host = host_is_search;
        if policy.forbid_search_engines && host_is_search {
            info!(
                policy = "agent_web.forbid_search_engines",
                dest_host = dest_host.as_deref().unwrap_or("<unknown>"),
                decision = "block",
                "blocked search-engine egress"
            );
            return RequestOrResponse::Response(mitm_blocked_response(
                "search engines disabled by [security.agent_web].forbid_search_engines",
            ));
        }

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
                return RequestOrResponse::Response(mitm_blocked_response(&format!(
                    "Substituted URL is not a valid URI: {err}"
                )));
            }
        };

        if let Err(err) =
            substitute_headers(&self.state, &mut parts.headers, dest_host.as_deref()).await
        {
            // Echoing `err` would leak the secret name in the block
            // page (the resolver/allowlist error includes the literal
            // ref name). Log details server-side, return a bland
            // explanation to the caller — same pattern as the original
            // axum blocked_response.
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
                // Bland message; the err text may contain the secret name
                // (resolver / allowlist failures include the literal ref).
                warn!("BLOCKED: MITM body substitution failed: {err}");
                return RequestOrResponse::Response(mitm_blocked_response("Request rejected"));
            }
        };

        // (C) Provider-browsing strip / block — only when body looks
        // like a JSON LLM request to a known LLM API.
        let body_bytes = {
            let policy = &self.state.config.agent_web;
            let dest = dest_host.as_deref().unwrap_or("<unknown>");
            let is_llm_api = dest_host
                .as_deref()
                .map(|h| host_is_known_llm_api(h, &policy.known_llm_apis))
                .unwrap_or(false);
            let looks_json = content_type
                .as_deref()
                .map(looks_like_json_content_type)
                .unwrap_or(false);
            if is_llm_api && looks_json && !body_bytes.is_empty() {
                match agent_web::inspect_browsing_body(&body_bytes, policy, dest) {
                    BrowsingDecision::Allow => body_bytes,
                    BrowsingDecision::Stripped { body, .. } => Bytes::from(body),
                    BrowsingDecision::Block { reason } => {
                        return RequestOrResponse::Response(mitm_blocked_response(&reason));
                    }
                }
            } else {
                body_bytes
            }
        };

        // (D) URL pre-flight — scan messages / tool descriptions for
        // URLs whose host is on the agent_web URL denylist. Same gate
        // as (C): only fires for JSON-shaped LLM requests.
        {
            let policy = &self.state.config.agent_web;
            let is_llm_api = dest_host
                .as_deref()
                .map(|h| host_is_known_llm_api(h, &policy.known_llm_apis))
                .unwrap_or(false);
            let looks_json = content_type
                .as_deref()
                .map(looks_like_json_content_type)
                .unwrap_or(false);
            if is_llm_api && looks_json && !body_bytes.is_empty() {
                if let Some(host) = agent_web::preflight_message_urls(&body_bytes, policy) {
                    info!(
                        policy = "agent_web.preflight_message_urls",
                        dest_host = dest_host.as_deref().unwrap_or("<unknown>"),
                        denied_host = host.as_str(),
                        decision = "block",
                        "blocked LLM request: references forbidden URL"
                    );
                    return RequestOrResponse::Response(mitm_blocked_response(&format!(
                        "request references forbidden URL: {host}"
                    )));
                }
            }
        }

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
                return mitm_blocked_response(&format!(
                    "Failed to decode upstream response: {err}"
                ));
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

        // (B) Search-response scanning. Runs only when the originating
        // request hit a host matched by `search_engine_patterns`. Two
        // passes here:
        //   1. Adversary scanner for prompt-injection content. Search
        //      APIs return JSON, which the generic `text/*` filter
        //      below skips — but those JSON snippets carry indexed
        //      page content that's the most common prompt-injection
        //      vector for an agent that "summarizes a URL". Run the
        //      scanner explicitly here regardless of content-type.
        //   2. Denylist check / strip via `scan_search_response`.
        let body_bytes = if self.last_was_search_host {
            let policy = &self.state.config.agent_web;
            let dest = self
                .last_url
                .as_deref()
                .and_then(|u| reqwest::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_owned());

            // Pass 1: prompt-injection scan on the (likely JSON) body.
            if self.state.config.scan_inbound {
                if let Ok(body_str) = std::str::from_utf8(&body_bytes) {
                    let verdict = self
                        .state
                        .scanner
                        .scan(target_url, body_str, ScanContext::WebFetch)
                        .await;
                    match verdict {
                        adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                            warn!(
                                policy = "agent_web.scan_search_responses",
                                dest_host = %dest,
                                reason = %reason,
                                "blocked search response: prompt-injection content"
                            );
                            return mitm_blocked_response(&format!(
                                "Search response blocked by prompt-injection scanner: {reason}"
                            ));
                        }
                        adversary_detector::verdict::ScanVerdict::Review { reason } => {
                            info!(
                                policy = "agent_web.scan_search_responses",
                                dest_host = %dest,
                                reason = %reason,
                                "REVIEW search response from search API"
                            );
                        }
                        adversary_detector::verdict::ScanVerdict::Clean => {}
                    }
                }
            }

            // Pass 2: denylist check / strip via `scan_search_response`.
            match agent_web::scan_search_response(&body_bytes, policy, &dest) {
                SearchResponseDecision::Pass => body_bytes,
                SearchResponseDecision::Block { reason } => {
                    return mitm_blocked_response(&reason);
                }
                SearchResponseDecision::Strip { body, .. } => Bytes::from(body),
            }
        } else {
            body_bytes
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
        let (status, value) = vault_json_response(headers, secret_name).await;
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

fn looks_like_json_content_type(ct: &str) -> bool {
    looks_like_json_content_type_pub(ct)
}

/// Public wrapper for `looks_like_json_content_type` so the
/// `proxy::intercept` axum handler can reuse the exact same content-type
/// classification as the MITM path.
pub fn looks_like_json_content_type_pub(ct: &str) -> bool {
    let head = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    head == "application/json" || head.ends_with("+json")
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

/// Build a block response that an LLM agent can read and reason about.
///
/// Returns HTTP 200 with an HTML body so that downstream agent tooling that
/// only checks `response.ok` still surfaces the explanation to the model.
/// The fetch *succeeded* in the protocol sense; the page content explains
/// that the operator's security gateway intercepted and refused the request.
/// Structured signals are also exposed via `X-Calciforge-*` headers so
/// non-LLM tooling can branch on the block without parsing HTML.
fn mitm_blocked_response(reason: &str) -> Response<MitmBody> {
    let escaped = html_escape(reason);
    let html = format!(
        "<!DOCTYPE html>\n\
         <html><head><meta charset=\"utf-8\">\
         <title>Page blocked by Calciforge security gateway</title></head>\
         <body>\
         <h1>Page blocked by Calciforge security gateway</h1>\
         <p><strong>Reason:</strong> {escaped}</p>\
         <h2>What this means</h2>\
         <p>This URL or response was blocked by the operator's security policy. \
         The original content has not been delivered to the agent. There is no \
         payload to evaluate; treat this as if the page were unavailable.</p>\
         <h2>Suggested next steps</h2>\
         <ul>\
         <li>Look for the same information on a different source.</li>\
         <li>If you specifically need this URL, ask the operator to allowlist it \
         or to relax the scanner rule that triggered.</li>\
         <li>Do not attempt to bypass the gateway via another proxy or tool — \
         every attempt is recorded in the audit log.</li>\
         </ul>\
         </body></html>"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("X-Calciforge-Blocked", "true")
        .header("X-Calciforge-Reason", sanitize_for_header(reason))
        .body(MitmBody::from(html))
        .unwrap_or_else(|_| {
            // Last-resort fallback if the builder above somehow fails (e.g. an
            // unexpected header value). Plain-text 200 keeps the agent-friendly
            // shape: still a successful fetch, still a readable explanation.
            Response::new(MitmBody::from(
                "Page blocked by Calciforge security gateway.\n",
            ))
        })
}

/// Strip everything that's not safe to put in an HTTP header value.
///
/// Per RFC 7230 §3.2, a header value is `*( field-vchar / SP / HTAB )`,
/// where `field-vchar = VCHAR (printable ASCII, %x21-%x7E)`. Anything
/// outside that range — control characters (NUL, BEL, ESC, DEL, …),
/// CR/LF, or any non-ASCII byte — either gets the response builder to
/// reject the header (and we lose the structured signal entirely) or,
/// worse, opens up header-injection if a CR/LF sneaks through.
///
/// This filter keeps printable ASCII (`0x20-0x7E`) and HTAB (`0x09`),
/// replacing everything else with a space. That covers NUL/CR/LF, DEL,
/// every C0/C1 control, and all UTF-8 bytes.
fn sanitize_for_header(s: &str) -> String {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            if cp == 0x09 || (0x20..=0x7E).contains(&cp) {
                c
            } else {
                ' '
            }
        })
        .collect()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response<MitmBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(MitmBody::from(value.to_string()))
        .unwrap_or_else(|_| mitm_blocked_response("Failed to build response"))
}

/// Env var holding the bearer token required to call `/vault/:secret`.
/// Unset → the vault route returns 503 (refuses to act as an oracle).
/// This is intentionally separate from any cred-injection token; it
/// guards the resolve-and-return path that has no other authn.
pub(crate) const VAULT_TOKEN_ENV: &str = "SECURITY_PROXY_VAULT_TOKEN";

/// Constant-time byte comparison to keep the bearer-token check from
/// leaking length/prefix information via timing. Std doesn't provide
/// one; we keep it tiny rather than pull a crate.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Resolve a secret for the `GET /vault/:secret` control-plane route
/// (now served only by the MITM handler — the plain-HTTP forward proxy
/// was removed in 2026-04).
///
/// Returns the (status, json) tuple for the caller to wrap in the
/// hudsucker `Response<MitmBody>` envelope. Neither the response body
/// nor ops logs contain the resolver's raw error text: a verbose error
/// would name the env vars probed and the vault URL queried, either of
/// which reveals shape of the secret store to anyone reading logs.
/// We log the secret *name* at `debug!` so you can correlate requests
/// to attempts during incident investigation, but the underlying error
/// stays redacted.
pub(crate) async fn vault_json_response(
    headers: &header::HeaderMap,
    secret_name: String,
) -> (StatusCode, serde_json::Value) {
    use tracing::debug;

    // Defense in depth: the binary defaults to binding 127.0.0.1 (see
    // main.rs), but if an operator opens it up to 0.0.0.0 the vault
    // route would otherwise be an unauthenticated secret oracle for
    // anyone on the network. Require a bearer token; if the env var is
    // unset, refuse to serve the route at all rather than silently
    // accepting "no token".
    match std::env::var(VAULT_TOKEN_ENV) {
        Ok(expected) if !expected.is_empty() => {
            let provided = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("");
            if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                debug!(secret = %secret_name, "vault auth failed");
                return (
                    StatusCode::UNAUTHORIZED,
                    serde_json::json!({"status": "error", "message": "unauthorized"}),
                );
            }
        }
        _ => {
            debug!(
                "vault route called but {} unset; refusing as oracle",
                VAULT_TOKEN_ENV
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({"status": "error", "message": "vault route disabled"}),
            );
        }
    }

    match secrets_client::vault::get_secret(&secret_name).await {
        Ok(token) => {
            debug!(secret = %secret_name, "vault route resolved secret");
            (
                StatusCode::OK,
                serde_json::json!({
                    "status": "ok",
                    "secret": secret_name,
                    "token": token,
                }),
            )
        }
        Err(_) => {
            // Name only; no error text. If you need to debug, enable
            // `RUST_LOG=secrets_client=debug` to see the underlying
            // resolver's own debug output.
            debug!(secret = %secret_name, "vault lookup failed");
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({
                    "status": "error",
                    "message": "Secret not found",
                }),
            )
        }
    }
}
