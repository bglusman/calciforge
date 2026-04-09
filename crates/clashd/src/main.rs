use axum::{
    routing::post,
    Json, Router,
    extract::State,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
struct EvaluateRequest {
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    context: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct EvaluateResponse {
    verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone)]
struct AppState {
    policy_path: PathBuf,
}

/// Hardcoded critical operations that ALWAYS require custodian review
const ALWAYS_REVIEW: &[(&str, &str)] = &[
    ("gateway", "config."),      // Any gateway config change
    ("gateway", "restart"),      // Gateway restart
    ("cron", "remove"),          // Cron job removal
    ("write", ".openclaw"),      // Writing to OpenClaw config
    ("edit", ".openclaw"),       // Editing OpenClaw config
];

async fn evaluate(
    State(state): State<AppState>,
    Json(req): Json<EvaluateRequest>,
) -> Result<Json<EvaluateResponse>, StatusCode> {
    info!(tool = %req.tool, "Policy evaluation request");
    
    // Serialize args to string for pattern matching
    let args_str = req.args.to_string();
    let tool_lower = req.tool.to_lowercase();
    
    // Check against ALWAYS_REVIEW list
    for (tool_pattern, args_pattern) in ALWAYS_REVIEW {
        if tool_lower.contains(tool_pattern) && args_str.contains(args_pattern) {
            warn!(
                tool = %req.tool,
                matched_tool = tool_pattern,
                matched_args = args_pattern,
                "REVIEW REQUIRED: Critical operation detected"
            );
            return Ok(Json(EvaluateResponse {
                verdict: "review".to_string(),
                reason: Some(format!(
                    "Critical operation '{} {}' requires custodian approval",
                    req.tool, args_pattern
                )),
            }));
        }
    }
    
    // Check for destructive operations
    let destructive_patterns = ["rm -rf", "rm -fr", "mkfs", "wipefs", "dd if=/dev/"];
    for pattern in &destructive_patterns {
        if args_str.contains(pattern) {
            warn!(pattern, "Destructive command detected");
            return Ok(Json(EvaluateResponse {
                verdict: "deny".to_string(),
                reason: Some(format!("Destructive command '{}' blocked by policy", pattern)),
            }));
        }
    }
    
    // Default: allow
    Ok(Json(EvaluateResponse {
        verdict: "allow".to_string(),
        reason: None,
    }))
}

async fn health() -> &'static str {
    "OK"
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "clashd",
        "version": env!("CARGO_PKG_VERSION"),
        "policy_engine": "hardcoded-v1",
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    let port = std::env::var("CLASHD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9001);
    
    let policy_path = std::env::var("CLASHD_POLICY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".clash").join("policy.star"))
                .unwrap_or_else(|| PathBuf::from("/etc/clash/policy.star"))
        });
    
    info!("╔══════════════════════════════════════════╗");
    info!("║           clashd v{}               ║", env!("CARGO_PKG_VERSION"));
    info!("║   Policy sidecar for OpenClaw           ║");
    info!("╚══════════════════════════════════════════╝");
    info!("");
    info!("Configuration:");
    info!("  Port: {}", port);
    info!("  Policy path: {:?}", policy_path);
    info!("  Critical operations requiring review:");
    for (tool, pattern) in ALWAYS_REVIEW {
        info!("    - {}:{}", tool, pattern);
    }
    
    let state = AppState { policy_path };
    
    let app = Router::new()
        .route("/", axum::routing::get(version))
        .route("/health", axum::routing::get(health))
        .route("/evaluate", post(evaluate))
        .with_state(state);
    
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("");
    info!("🚀 Listening on http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}
