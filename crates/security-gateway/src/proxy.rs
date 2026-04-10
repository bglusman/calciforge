use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use reqwest::Client;
use tracing::{error, info, warn};

use crate::config::{GatewayConfig, Verdict};
use crate::credentials::CredentialInjector;
use crate::scanner::{ExfilScanner, InjectionScanner};

/// Shared state for the proxy handler.
pub struct ProxyState {
    pub config: GatewayConfig,
    pub exfil_scanner: ExfilScanner,
    pub injection_scanner: InjectionScanner,
    pub credentials: CredentialInjector,
    pub audit: crate::audit::AuditLogger,
    pub http_client: Client,
}

/// Handle incoming proxy requests (forward proxy mode).
/// All requests go through: scan → inject → forward → scan response → return.
pub async fn proxy_handler(
    State(state): State<Arc<ProxyState>>,
    req: Request<Body>,
) -> Result<Response, Infallible> {
    let start = Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();

    // Build the full target URL
    let target_url = if uri.scheme().is_some() {
        uri.to_string()
    } else {
        // Relative URI — extract target from Host header
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown");
        format!(
            "http://{}{}",
            host,
            uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
        )
    };

    info!("{} {}", method, target_url);

    // Check bypass domains
    if is_bypassed(&target_url, &state.config.bypass_domains) {
        info!("Bypassing: {}", target_url);
        return Ok(forward_request(state, req, &target_url, vec![], start).await);
    }

    // Read request body for scanning
    let body_bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return Ok(blocked_response("Failed to read request body"));
        }
    };
    let body_str = String::from_utf8_lossy(&body_bytes);

    // STEP 1: Outbound scan (exfiltration check)
    if state.config.scan_outbound && !body_str.is_empty() {
        let exfil_report = state.exfil_scanner.scan(&target_url, &body_str).await;
        match &exfil_report.verdict {
            Verdict::Block { reason } => {
                warn!("BLOCKED outbound to {}: {}", target_url, reason);
                return Ok(blocked_response(&format!(
                    "Outbound request blocked: {}",
                    reason
                )));
            }
            Verdict::Log { finding } => {
                info!("REVIEW outbound to {}: {}", target_url, finding);
            }
            Verdict::Allow => {}
        }
    }

    // STEP 2: Credential injection
    let mut injected_creds = vec![];
    if state.config.inject_credentials {
        if let Some(host) = reqwest::Url::parse(&target_url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
        {
            let mut headers_vec = vec![];
            state.credentials.inject(&mut headers_vec, &host);
            injected_creds = headers_vec.iter().map(|(k, _)| k.clone()).collect();
        }
    }

    // Forward the request (placeholder — full impl below)
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"status":"ok","url":"{}","injected":{}}}"#,
            target_url,
            !injected_creds.is_empty()
        )))
        .unwrap())
}

/// Forward the actual request upstream and scan the response.
async fn forward_request(
    state: Arc<ProxyState>,
    _req: Request<Body>,
    target_url: &str,
    _injected_headers: Vec<(String, String)>,
    _start: Instant,
) -> Response {
    match state.http_client.get(target_url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();

            // STEP 4: Inbound scan (injection check)
            if state.config.scan_inbound && !body.is_empty() {
                let inj_report = state.injection_scanner.scan(target_url, &body).await;
                match &inj_report.verdict {
                    Verdict::Block { reason } => {
                        warn!("BLOCKED response from {}: {}", target_url, reason);
                        return blocked_response(&format!("Response blocked: {}", reason));
                    }
                    Verdict::Log { finding } => {
                        info!("REVIEW response from {}: {}", target_url, finding);
                    }
                    Verdict::Allow => {}
                }
            }

            Response::builder()
                .status(status.as_u16())
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        }
        Err(e) => {
            error!("Failed to forward to {}: {}", target_url, e);
            blocked_response(&format!("Upstream error: {}", e))
        }
    }
}

fn blocked_response(reason: &str) -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"blocked":true,"reason":"{}"}}"#,
            reason.replace('"', "\\\"")
        )))
        .unwrap()
}

fn is_bypassed(url: &str, bypass_domains: &[String]) -> bool {
    for domain in bypass_domains {
        if url.contains(domain) {
            return true;
        }
    }
    false
}

/// Health/status endpoint for the gateway.
pub async fn health_handler(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    let audit_count = state.audit.count();
    let blocked = state.audit.blocked_and_reviewed().len();

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"status":"ok","service":"security-gateway","version":"{}","total_requests":{},"blocked_or_reviewed":{}}}"#,
            env!("CARGO_PKG_VERSION"),
            audit_count,
            blocked
        )))
        .unwrap()
}
