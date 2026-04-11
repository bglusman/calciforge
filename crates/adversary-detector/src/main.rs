use adversary_detector::{AdversaryScanner, ScanContext, ScanVerdict, ScannerConfig};
use axum::{
    extract::Json,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

#[derive(Deserialize)]
struct PeekRequest {
    url: String,
}

#[derive(Deserialize)]
struct ScanRequest {
    content: String,
    url: Option<String>,
}

/// Health check endpoint
async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "adversary-detector",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Peek at a URL (structural + semantic checks)
async fn peek(Json(req): Json<PeekRequest>) -> impl IntoResponse {
    info!("Peek request for: {}", req.url);
    Json(json!({
        "url": req.url,
        "status": "not_implemented",
        "verdict": "review",
        "reason": "URL fetching not yet implemented in Rust binary",
    }))
}

/// Scan raw content directly
async fn scan(Json(req): Json<ScanRequest>) -> impl IntoResponse {
    let config = ScannerConfig::default();
    let scanner = AdversaryScanner::new(config);
    let url = req.url.as_deref().unwrap_or("unknown");
    let ctx = ScanContext::Api;

    let verdict = scanner.scan(url, &req.content, ctx).await;
    let (verdict_str, reason) = match &verdict {
        ScanVerdict::Clean => ("clean", None),
        ScanVerdict::Review { reason } => ("review", Some(reason.clone())),
        ScanVerdict::Unsafe { reason } => ("unsafe", Some(reason.clone())),
    };

    let mut response = json!({
        "verdict": verdict_str,
        "url": url,
    });
    if let Some(r) = reason {
        response["reason"] = json!(r);
    }
    Json(response)
}

/// Scan content for injection (POST body)
async fn scan_injection(body: String) -> impl IntoResponse {
    let config = ScannerConfig::default();
    let scanner = AdversaryScanner::new(config);
    let ctx = ScanContext::Exec;

    let verdict = scanner.scan("inline", &body, ctx).await;
    let verdict_str = match &verdict {
        ScanVerdict::Clean => "clean",
        ScanVerdict::Review { .. } => "review",
        ScanVerdict::Unsafe { .. } => "unsafe",
    };

    Json(json!({
        "verdict": verdict_str,
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "adversary_detector=info".into()),
        )
        .init();

    let port: u16 = std::env::var("ADVERSARY_DETECTOR_PORT")
        .unwrap_or_else(|_| "9800".into())
        .parse()
        .unwrap_or(9800);

    let app = Router::new()
        .route("/health", get(health))
        .route("/peek", post(peek))
        .route("/scan", post(scan))
        .route("/scan/injection", post(scan_injection));

    let addr = format!("127.0.0.1:{}", port);
    info!("adversary-detector listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
