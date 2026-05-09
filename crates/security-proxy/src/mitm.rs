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
use anyhow::{Context, Result, anyhow};
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
    self, BrowsingDecision, SearchResponseDecision, host_is_known_llm_api,
    host_matches_search_engine,
};
use crate::credentials::CredentialInjection;
use crate::proxy::{self, BodyMode, SecurityProxy, redact_url_for_log};

static CRYPTO_PROVIDER_INIT: Once = Once::new();

const CALCIFORGE_OVERRIDE_HEADER: &str = "x-calciforge-override";
const MANUAL_CREDENTIAL_POLICY: &str = "ironclaw.manual_credential";
const MANUAL_CREDENTIAL_OVERRIDE_TOKEN_ENV: &str =
    "SECURITY_PROXY_MANUAL_CREDENTIAL_OVERRIDE_TOKEN";

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
        let original_target_url = match request_target_url(&req) {
            Some(url) => url,
            None => {
                warn!("BLOCKED: MITM request target is not reconstructable");
                return RequestOrResponse::Response(mitm_blocked_response(
                    "Request URL could not be reconstructed; the gateway refuses requests \
                     it cannot identify a destination for.",
                ));
            }
        };
        info!(
            "MITM {} {}",
            method,
            redact_url_for_log(&original_target_url)
        );

        let url_dest_host = reqwest::Url::parse(&original_target_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        if url_dest_host.is_none() && original_target_url.contains("{{secret:") {
            warn!("BLOCKED: MITM URL contains secret ref but host is unparseable");
            return RequestOrResponse::Response(mitm_blocked_response(
                "URL contains a secret reference but the host portion could not be parsed; \
                 the gateway refuses to substitute secrets without a known destination.",
            ));
        }

        // IronClaw credential-injection detection: check BEFORE any
        // Calciforge secret substitution or CredentialInjector changes.
        // At this point URL/header values are still agent-supplied and may
        // contain either:
        // - manual credentials (bad — block these)
        // - {{secret:...}} placeholders (good — proxy-managed injection)
        #[cfg(feature = "ironclaw-safety")]
        let manual_credential_override = manual_credential_override_status(
            req.headers(),
            self.state
                .config
                .manual_credential_override_requires_operator_approval,
        );

        #[cfg(feature = "ironclaw-safety")]
        {
            let request_params = build_credential_check_params(&original_target_url, req.headers());
            if let Err(reason) = self
                .state
                .ironclaw
                .check_request_credentials(&request_params)
            {
                warn!(
                    "BLOCKED MITM request to {}: {}",
                    redact_url_for_log(&original_target_url),
                    reason
                );
                if manual_credential_override.allowed {
                    warn!(
                        "OVERRIDE: allowed {} for MITM request to {} ({})",
                        MANUAL_CREDENTIAL_POLICY,
                        redact_url_for_log(&original_target_url),
                        manual_credential_override.reason
                    );
                } else {
                    return RequestOrResponse::Response(mitm_manual_credential_blocked_response(
                        &reason,
                        &original_target_url,
                    ));
                }
            }
        }

        let mut secret_metadata = None;
        if original_target_url.contains("{{secret:")
            && let Some(host) = url_dest_host.as_deref()
        {
            match SecurityProxy::load_secret_metadata(host) {
                Ok(metadata) => secret_metadata = Some(metadata),
                Err(err) => {
                    warn!("BLOCKED: MITM URL substitution failed: {err}");
                    return RequestOrResponse::Response(mitm_policy_blocked_response(
                        "secret_substitution.url",
                        "URL secret substitution failed. Check the secret exists and is allowed for this destination.",
                        "config_required",
                        "none",
                    ));
                }
            }
        }

        let target_url = match self
            .state
            .resolve_and_substitute(
                &original_target_url,
                url_dest_host.as_deref(),
                secret_metadata.as_ref(),
            )
            .await
        {
            Ok(url) => url,
            Err(err) => {
                // Bland message; the err text contains the secret name.
                warn!("BLOCKED: MITM URL substitution failed: {err}");
                return RequestOrResponse::Response(mitm_policy_blocked_response(
                    "secret_substitution.url",
                    "URL secret substitution failed. Check the secret exists and is allowed for this destination.",
                    "config_required",
                    "none",
                ));
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
            return RequestOrResponse::Response(mitm_policy_blocked_response(
                "agent_web.forbid_search_engines",
                "search engines disabled by [security.agent_web].forbid_search_engines",
                "config_required",
                "none",
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

        if let Err(err) = substitute_headers(
            &self.state,
            &mut parts.headers,
            dest_host.as_deref(),
            &mut secret_metadata,
        )
        .await
        {
            warn!("BLOCKED: MITM header substitution failed: {err}");
            return RequestOrResponse::Response(mitm_policy_blocked_response(
                "secret_substitution.header",
                "Header secret substitution failed. Check the secret exists and is allowed for this destination.",
                "config_required",
                "none",
            ));
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
            &mut secret_metadata,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(err) => {
                // Bland message; the err text may contain the secret name
                // (resolver / allowlist failures include the literal ref).
                warn!("BLOCKED: MITM body substitution failed: {err}");
                return RequestOrResponse::Response(mitm_policy_blocked_response(
                    "secret_substitution.body",
                    "Body secret substitution failed. Check the secret exists, content type is supported, and destination is allowed.",
                    "config_required",
                    "none",
                ));
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
                        return RequestOrResponse::Response(mitm_policy_blocked_response(
                            "agent_web.forbid_provider_browsing",
                            &reason,
                            "config_required",
                            "none",
                        ));
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
            if is_llm_api
                && looks_json
                && !body_bytes.is_empty()
                && let Some(host) = agent_web::preflight_message_urls(&body_bytes, policy)
            {
                info!(
                    policy = "agent_web.preflight_message_urls",
                    dest_host = dest_host.as_deref().unwrap_or("<unknown>"),
                    denied_host = host.as_str(),
                    decision = "block",
                    "blocked LLM request: references forbidden URL"
                );
                return RequestOrResponse::Response(mitm_policy_blocked_response(
                    "agent_web.preflight_message_urls",
                    &format!("request references forbidden URL host: {host}"),
                    "config_required",
                    "none",
                ));
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
                .scan(
                    &redact_url_for_log(&target_url),
                    &body_text,
                    ScanContext::Api,
                )
                .await;
            match verdict {
                adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                    warn!(
                        "BLOCKED MITM outbound to {}: {}",
                        redact_url_for_log(&target_url),
                        reason
                    );
                    return RequestOrResponse::Response(mitm_policy_blocked_response(
                        "scanner.outbound_exfiltration",
                        &format!("Outbound request blocked: {reason}"),
                        "config_required",
                        "none",
                    ));
                }
                adversary_detector::verdict::ScanVerdict::Review { reason } => {
                    info!(
                        "REVIEW MITM outbound to {}: {}",
                        redact_url_for_log(&target_url),
                        reason
                    );
                }
                adversary_detector::verdict::ScanVerdict::Clean => {}
            }
        }

        if self.state.config.inject_credentials
            && let Some(host) = dest_host.as_deref()
        {
            let injections = self.state.credentials.injections_for_host(host).await;
            for injection in injections {
                match injection {
                    CredentialInjection::Header { name, value } => {
                        if let (Ok(name), Ok(value)) = (
                            header::HeaderName::try_from(name.as_str()),
                            header::HeaderValue::try_from(value.as_str()),
                        ) {
                            parts.headers.insert(name, value);
                        }
                    }
                    CredentialInjection::QueryParam { name, value } => {
                        if let Err(err) = append_query_param_to_uri(&mut parts.uri, &name, &value) {
                            warn!("BLOCKED: MITM credential query-param injection failed: {err}");
                            return RequestOrResponse::Response(mitm_policy_blocked_response(
                                "credential_injection.query_param",
                                "Credential query-parameter injection failed before forwarding.",
                                "config_required",
                                "none",
                            ));
                        }
                    }
                }
            }
        }

        remove_calciforge_control_headers(&mut parts.headers);
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
            if self.state.config.scan_inbound
                && let Ok(body_str) = std::str::from_utf8(&body_bytes)
            {
                let verdict = self
                    .state
                    .scanner
                    .scan(
                        &redact_url_for_log(target_url),
                        body_str,
                        ScanContext::WebFetch,
                    )
                    .await;
                match verdict {
                    adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                        warn!(
                            policy = "agent_web.scan_search_responses",
                            dest_host = %dest,
                            reason = %reason,
                            "blocked search response: prompt-injection content"
                        );
                        return mitm_policy_blocked_response(
                            "agent_web.scan_search_responses",
                            &format!(
                                "Search response blocked by prompt-injection scanner: {reason}"
                            ),
                            "config_required",
                            "none",
                        );
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

            // Pass 2: denylist check / strip via `scan_search_response`.
            match agent_web::scan_search_response(&body_bytes, policy, &dest) {
                SearchResponseDecision::Pass => body_bytes,
                SearchResponseDecision::Block { reason } => {
                    return mitm_policy_blocked_response(
                        "agent_web.scan_search_responses",
                        &reason,
                        "config_required",
                        "none",
                    );
                }
                SearchResponseDecision::Strip { body, .. } => Bytes::from(body),
            }
        } else {
            body_bytes
        };

        if self.state.config.scan_inbound
            && looks_like_scannable_content_type(&content_type)
            && let Ok(body_str) = std::str::from_utf8(&body_bytes)
        {
            // IronClaw leak detection (runs before adversary-detector scan)
            #[cfg(feature = "ironclaw-safety")]
            {
                if let Err(reason) = self.state.ironclaw.scan_response_body(body_str) {
                    warn!(
                        "BLOCKED MITM response from {}: {}",
                        redact_url_for_log(target_url),
                        reason
                    );
                    return mitm_policy_blocked_response(
                        "ironclaw.response_secret_leak",
                        &reason,
                        "config_required",
                        "none",
                    );
                }
            }

            let verdict = self
                .state
                .scanner
                .scan(
                    &redact_url_for_log(target_url),
                    body_str,
                    ScanContext::WebFetch,
                )
                .await;
            match verdict {
                adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                    warn!(
                        "BLOCKED MITM response from {}: {}",
                        redact_url_for_log(target_url),
                        reason
                    );
                    return mitm_policy_blocked_response(
                        "scanner.inbound_prompt_injection",
                        &format!("Response blocked: {reason}"),
                        "config_required",
                        "none",
                    );
                }
                adversary_detector::verdict::ScanVerdict::Review { reason } => {
                    info!(
                        "REVIEW MITM response from {}: {}",
                        redact_url_for_log(target_url),
                        reason
                    );
                }
                adversary_detector::verdict::ScanVerdict::Clean => {}
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
    metadata: &mut Option<secrets_client::SecretMetadataStore>,
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
        if value_str.contains("{{secret:")
            && let Some(host) = dest_host
            && metadata.is_none()
        {
            *metadata = Some(SecurityProxy::load_secret_metadata(host)?);
        }
        let substituted = state
            .resolve_and_substitute(value_str, dest_host, metadata.as_ref())
            .await?;
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
    metadata: &mut Option<secrets_client::SecretMetadataStore>,
) -> Result<Bytes, String> {
    if body_bytes.is_empty() {
        return Ok(body_bytes);
    }

    match SecurityProxy::body_substitution_mode(content_type) {
        BodyMode::FullSubstitute => {
            let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
            if body_str.contains("{{secret:")
                && let Some(host) = dest_host
                && metadata.is_none()
            {
                *metadata = Some(SecurityProxy::load_secret_metadata(host)?);
            }
            state
                .resolve_and_substitute(&body_str, dest_host, metadata.as_ref())
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

fn looks_like_scannable_content_type(ct: &str) -> bool {
    looks_like_scannable_content_type_pub(ct)
}

fn append_query_param_to_uri(uri: &mut http::Uri, name: &str, value: &str) -> Result<(), String> {
    let mut url = reqwest::Url::parse(&uri.to_string()).map_err(|err| err.to_string())?;
    url.query_pairs_mut().append_pair(name, value);
    *uri = url
        .as_str()
        .parse()
        .map_err(|err| format!("query-param URI parse failed: {err}"))?;
    Ok(())
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

/// Public wrapper for response-body types that are safe and useful to scan as
/// UTF-8. This intentionally includes JSON and common structured text formats
/// because provider/search/tool APIs often return prompt-bearing content as
/// `application/json`, not `text/*`.
pub fn looks_like_scannable_content_type_pub(ct: &str) -> bool {
    let head = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    head.starts_with("text/")
        || head == "application/json"
        || head.ends_with("+json")
        || head == "application/javascript"
        || head == "application/x-javascript"
        || head == "application/xml"
        || head.ends_with("+xml")
        || head == "application/xhtml+xml"
        || head == "image/svg+xml"
        || head == "application/x-www-form-urlencoded"
        || head == "application/graphql"
        || head == "application/yaml"
        || head == "application/x-yaml"
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

fn remove_calciforge_control_headers(headers: &mut header::HeaderMap) {
    let control_names: Vec<header::HeaderName> = headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-calciforge-"))
        .cloned()
        .collect();
    for name in control_names {
        headers.remove(name);
    }
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualCredentialOverrideStatus {
    allowed: bool,
    reason: String,
}

#[cfg(feature = "ironclaw-safety")]
fn manual_credential_override_status(
    headers: &header::HeaderMap,
    requires_operator_approval: bool,
) -> ManualCredentialOverrideStatus {
    let Some(value) = headers
        .get(CALCIFORGE_OVERRIDE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    else {
        return ManualCredentialOverrideStatus {
            allowed: false,
            reason: "no override header supplied".into(),
        };
    };

    let (policy, token) = value
        .split_once(':')
        .map(|(policy, token)| (policy.trim(), Some(token.trim())))
        .unwrap_or((value, None));
    if policy != MANUAL_CREDENTIAL_POLICY {
        return ManualCredentialOverrideStatus {
            allowed: false,
            reason: "override header does not target ironclaw.manual_credential".into(),
        };
    }

    if !requires_operator_approval {
        return ManualCredentialOverrideStatus {
            allowed: true,
            reason: "operator approval disabled by configuration".into(),
        };
    }

    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return ManualCredentialOverrideStatus {
            allowed: false,
            reason: "operator approval token required".into(),
        };
    };
    match std::env::var(MANUAL_CREDENTIAL_OVERRIDE_TOKEN_ENV) {
        Ok(expected) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
            ManualCredentialOverrideStatus {
                allowed: true,
                reason: "operator approval token accepted".into(),
            }
        }
        Ok(_) => ManualCredentialOverrideStatus {
            allowed: false,
            reason: "operator approval token rejected".into(),
        },
        Err(_) => ManualCredentialOverrideStatus {
            allowed: false,
            reason: format!("{MANUAL_CREDENTIAL_OVERRIDE_TOKEN_ENV} is not configured"),
        },
    }
}

fn mitm_manual_credential_blocked_response(reason: &str, url: &str) -> Response<MitmBody> {
    let escaped_reason = html_escape(reason);
    let escaped_url = html_escape(&redact_url_for_log(url));
    let html = format!(
        "<!DOCTYPE html>\n\
         <html><head><meta charset=\"utf-8\">\
         <title>Calciforge blocked manually supplied credentials</title></head>\
         <body>\
         <h1>Calciforge blocked manually supplied credentials</h1>\
         <p><strong>Policy:</strong> ironclaw.manual_credential</p>\
         <p><strong>Reason:</strong> {escaped_reason}</p>\
         <p><strong>Destination:</strong> {escaped_url}</p>\
         <h2>What this means</h2>\
         <p>The request appeared to contain a credential supplied directly by the agent \
         in a URL, header, or other request parameter. Calciforge only allows credentials \
         to flow through proxy-managed mechanisms such as <code>{{{{secret:NAME}}}}</code> \
         unless the operator explicitly approves an override.</p>\
         <h2>Suggested next steps</h2>\
         <ul>\
         <li>Retry with a Calciforge secret reference, for example \
         <code>{{{{secret:API_KEY_NAME}}}}</code>, instead of a raw credential.</li>\
         <li>If this was a false positive or a legacy API genuinely requires this shape, \
         ask the operator to approve a scoped override.</li>\
         </ul>\
         <h2>Override</h2>\
         <p><strong>Operator approval required by default.</strong> Operators can issue a \
         scoped override with <code>X-Calciforge-Override</code>. Deployments may explicitly \
         configure this class of override to skip operator approval for trusted contexts, \
         but the default is fail-closed.</p>\
         </body></html>"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("X-Calciforge-Blocked", "true")
        .header("X-Calciforge-Policy", "ironclaw.manual_credential")
        .header("X-Calciforge-Reason", sanitize_for_header(reason))
        .header("X-Calciforge-Operator-Approval", "required")
        .header("X-Calciforge-Override-Supported", "operator_scoped")
        .header("X-Calciforge-Override-Header", "X-Calciforge-Override")
        .body(MitmBody::from(html))
        .unwrap_or_else(|_| {
            Response::new(MitmBody::from(
                "Calciforge blocked manually supplied credentials. Operator approval required.\n",
            ))
        })
}

fn mitm_policy_blocked_response(
    policy: &str,
    reason: &str,
    operator_approval: &str,
    override_supported: &str,
) -> Response<MitmBody> {
    let escaped_policy = html_escape(policy);
    let escaped_reason = html_escape(reason);
    let html = format!(
        "<!DOCTYPE html>\n\
         <html><head><meta charset=\"utf-8\">\
         <title>Page blocked by Calciforge security gateway</title></head>\
         <body>\
         <h1>Page blocked by Calciforge security gateway</h1>\
         <p><strong>Policy:</strong> {escaped_policy}</p>\
         <p><strong>Reason:</strong> {escaped_reason}</p>\
         <h2>What this means</h2>\
         <p>This request or response was blocked by Calciforge security policy. \
         The original content has not been delivered to the agent.</p>\
         <h2>Suggested next steps</h2>\
         <ul>\
         <li>If this is a secret placeholder issue, check the secret name, store, and destination allowlist.</li>\
         <li>If this is an agent-web or scanner policy block, ask the operator to adjust configuration or policy.</li>\
         <li>Do not attempt to bypass the gateway via another proxy or tool.</li>\
         </ul>\
         </body></html>"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("X-Calciforge-Blocked", "true")
        .header("X-Calciforge-Policy", sanitize_for_header(policy))
        .header("X-Calciforge-Reason", sanitize_for_header(reason))
        .header("X-Calciforge-Operator-Approval", operator_approval)
        .header("X-Calciforge-Override-Supported", override_supported)
        .body(MitmBody::from(html))
        .unwrap_or_else(|_| {
            Response::new(MitmBody::from(
                "Page blocked by Calciforge security gateway.\n",
            ))
        })
}

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

/// Build a JSON representation of request parameters suitable for
/// `ironclaw_safety::params_contain_manual_credentials`. Extracts the URL
/// and headers from the in-flight request parts.
#[cfg(feature = "ironclaw-safety")]
fn build_credential_check_params(url: &str, headers: &header::HeaderMap) -> serde_json::Value {
    let credential_check_url = url_with_secret_query_params_removed(url);
    let mut header_map = serde_json::Map::new();
    for (name, value) in headers.iter() {
        // proxy-authorization is a standard hop-by-hop header used to
        // authenticate with this proxy itself — always stripped before
        // forwarding, never LLM-injected.
        if name == header::PROXY_AUTHORIZATION || name.as_str().starts_with("x-calciforge-") {
            continue;
        }
        if let Ok(v) = value.to_str() {
            // Skip only fully proxy-managed credential header values. Mixed
            // manual+placeholder values stay visible to IronClaw.
            if header_value_is_proxy_managed_secret(v) {
                continue;
            }
            header_map.insert(
                name.as_str().to_owned(),
                serde_json::Value::String(v.to_owned()),
            );
        }
    }
    serde_json::json!({
        "url": credential_check_url,
        "headers": header_map,
    })
}

#[cfg(feature = "ironclaw-safety")]
fn url_with_secret_query_params_removed(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_owned();
    };

    let original_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    if original_pairs.is_empty() {
        return url.to_owned();
    }

    let mut removed_any = false;
    let kept_pairs: Vec<(String, String)> = original_pairs
        .into_iter()
        .filter(|(_, value)| {
            let is_proxy_managed_secret = is_exact_secret_reference(value);
            removed_any |= is_proxy_managed_secret;
            !is_proxy_managed_secret
        })
        .collect();

    if !removed_any {
        return url.to_owned();
    }

    parsed.set_query(None);
    if !kept_pairs.is_empty() {
        let mut query = parsed.query_pairs_mut();
        for (name, value) in kept_pairs {
            query.append_pair(&name, &value);
        }
    }
    parsed.to_string()
}

#[cfg(feature = "ironclaw-safety")]
fn header_value_is_proxy_managed_secret(value: &str) -> bool {
    let trimmed = value.trim();
    if is_exact_secret_reference(trimmed) {
        return true;
    }

    let Some((scheme, rest)) = trimmed.split_once(char::is_whitespace) else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "token" | "digest" | "hoba" | "mutual"
    ) && is_exact_secret_reference(rest.trim())
}

#[cfg(feature = "ironclaw-safety")]
fn is_exact_secret_reference(value: &str) -> bool {
    let Ok(names) = crate::substitution::find_refs(value) else {
        return false;
    };
    let mut names = names.into_iter();
    let Some(name) = names.next() else {
        return false;
    };
    names.next().is_none() && value == format!("{{{{secret:{name}}}}}")
}

#[cfg(all(test, feature = "ironclaw-safety"))]
mod credential_check_tests {
    use super::{
        CALCIFORGE_OVERRIDE_HEADER, MANUAL_CREDENTIAL_POLICY, build_credential_check_params,
        header_value_is_proxy_managed_secret, manual_credential_override_status,
        mitm_manual_credential_blocked_response, remove_calciforge_control_headers,
        url_with_secret_query_params_removed,
    };
    use hudsucker::hyper::header;

    #[test]
    fn credential_check_url_omits_proxy_managed_secret_query_params() {
        let url = "https://api.example.test/v1?api_key={{secret:EXAMPLE_API_KEY}}&q=books";
        let sanitized = url_with_secret_query_params_removed(url);

        assert_eq!(sanitized, "https://api.example.test/v1?q=books");
    }

    #[test]
    fn credential_check_still_flags_manual_query_credentials() {
        let headers = header::HeaderMap::new();
        let params = build_credential_check_params(
            "https://api.example.test/v1?api_key=manual-secret&q=books",
            &headers,
        );

        assert!(ironclaw_safety::params_contain_manual_credentials(&params));
    }

    #[test]
    fn credential_check_still_flags_mixed_manual_and_placeholder_query_credentials() {
        let headers = header::HeaderMap::new();
        let params = build_credential_check_params(
            "https://api.example.test/v1?api_key=manual-prefix-{{secret:EXAMPLE_API_KEY}}&q=books",
            &headers,
        );

        assert!(ironclaw_safety::params_contain_manual_credentials(&params));
    }

    #[test]
    fn credential_check_allows_secret_placeholder_query_credentials() {
        let headers = header::HeaderMap::new();
        let params = build_credential_check_params(
            "https://api.example.test/v1?api_key={{secret:EXAMPLE_API_KEY}}&q=books",
            &headers,
        );

        assert!(!ironclaw_safety::params_contain_manual_credentials(&params));
    }

    #[test]
    fn credential_check_allows_proxy_managed_auth_headers() {
        assert!(header_value_is_proxy_managed_secret(
            "{{secret:EXAMPLE_API_KEY}}"
        ));
        assert!(header_value_is_proxy_managed_secret(
            "Bearer {{secret:EXAMPLE_API_KEY}}"
        ));
    }

    #[test]
    fn credential_check_keeps_mixed_manual_and_placeholder_headers_visible() {
        assert!(!header_value_is_proxy_managed_secret(
            "Bearer manual-prefix-{{secret:EXAMPLE_API_KEY}}"
        ));
    }

    #[test]
    fn manual_credential_block_response_names_policy_and_override_requirement() {
        let response = mitm_manual_credential_blocked_response(
            "LLM-injected credential detected in outgoing request",
            "https://api.example.test/v1?api_key=redacted",
        );

        assert_eq!(
            response.headers()["X-Calciforge-Policy"],
            "ironclaw.manual_credential"
        );
        assert_eq!(
            response.headers()["X-Calciforge-Operator-Approval"],
            "required"
        );
        assert_eq!(
            response.headers()["X-Calciforge-Override-Supported"],
            "operator_scoped"
        );
        assert_eq!(
            response.headers()["X-Calciforge-Override-Header"],
            "X-Calciforge-Override"
        );
    }

    #[test]
    fn manual_credential_override_requires_operator_approval_by_default() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            CALCIFORGE_OVERRIDE_HEADER,
            header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
        );

        let status = manual_credential_override_status(&headers, true);
        assert!(!status.allowed);
        assert_eq!(status.reason, "operator approval token required");
    }

    #[test]
    fn manual_credential_override_can_be_configured_without_operator_approval() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            CALCIFORGE_OVERRIDE_HEADER,
            header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
        );

        let status = manual_credential_override_status(&headers, false);
        assert!(status.allowed);
    }

    #[test]
    fn calciforge_control_headers_are_stripped_before_forwarding() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "x-calciforge-override",
            header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
        );
        headers.insert(
            "x-calciforge-anything",
            header::HeaderValue::from_static("control-plane"),
        );
        headers.insert(
            "x-upstream-header",
            header::HeaderValue::from_static("keep"),
        );

        remove_calciforge_control_headers(&mut headers);

        assert!(!headers.contains_key("x-calciforge-override"));
        assert!(!headers.contains_key("x-calciforge-anything"));
        assert_eq!(headers["x-upstream-header"], "keep");
    }
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
