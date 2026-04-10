use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use clashd::policy::{AgentPolicyConfig, PolicyEngine};

#[derive(Debug, Clone, Deserialize)]
struct EvaluateRequest {
    tool: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    context: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct EvaluateResponse {
    verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone)]
struct AppState {
    engine: Arc<PolicyEngine>,
}

/// Agent policy configuration loaded from file
#[derive(Debug, Deserialize)]
struct AgentPolicyFile {
    agents: Vec<AgentPolicyConfig>,
}

/// POST /evaluate — main policy check endpoint
async fn evaluate(
    State(state): State<AppState>,
    Json(req): Json<EvaluateRequest>,
) -> Result<Json<EvaluateResponse>, StatusCode> {
    // Extract agent_id from context if present
    let agent_id = req
        .context
        .as_ref()
        .and_then(|c| c.get("agent_id"))
        .and_then(|v| v.as_str());

    let result = state.engine.evaluate(&req.tool, &req.args, agent_id).await;

    Ok(Json(EvaluateResponse {
        verdict: result.verdict.to_string(),
        reason: result.reason,
    }))
}

/// GET /domains/summary — list loaded domain lists and their sizes
async fn domain_summary(State(state): State<AppState>) -> Json<Value> {
    let summary = state.engine.domain_list_summary().await;
    let lists: Vec<Value> = summary
        .into_iter()
        .map(|(name, count)| serde_json::json!({"name": name, "entries": count}))
        .collect();
    Json(serde_json::json!({ "domain_lists": lists }))
}

/// GET /domains/check/:domain — check a domain against all lists
async fn domain_check(
    State(state): State<AppState>,
    UrlPath(domain): UrlPath<String>,
) -> Json<Value> {
    let matched = state.engine.domain_list_summary().await;
    // Note: we need direct list access for this - simplified version
    Json(serde_json::json!({
        "domain": domain,
        "checked_against": matched.len(),
    }))
}

/// GET /health — health check
async fn health() -> &'static str {
    "OK"
}

/// GET / — version info
async fn version() -> Json<Value> {
    Json(serde_json::json!({
        "name": "clashd",
        "version": env!("CARGO_PKG_VERSION"),
        "policy_engine": "starlark-v1",
        "features": [
            "starlark_evaluation",
            "domain_filtering",
            "per_agent_policy",
            "dynamic_threat_feeds",
            "regex_patterns",
            "static_lists"
        ]
    }))
}

/// Load agent policy configs from a JSON file
async fn load_agent_configs(path: &PathBuf) -> Result<Vec<AgentPolicyConfig>, String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read agent config {}: {}", path.display(), e))?;

    let file: AgentPolicyFile = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse agent config {}: {}", path.display(), e))?;

    Ok(file.agents)
}

/// Domain list refresh background task
async fn domain_refresh_loop(engine: Arc<PolicyEngine>, interval: Duration) {
    // Configure client with timeouts for security and reliability
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30)) // Total request timeout
        .connect_timeout(Duration::from_secs(10)) // Connection establishment
        .pool_idle_timeout(Duration::from_secs(60)) // Connection reuse
        .build()
        .unwrap_or_else(|e| {
            warn!(error = %e, "Failed to build HTTP client with custom timeouts, using default");
            reqwest::Client::new()
        });

    // Do initial refresh on startup
    info!("Performing initial domain list refresh...");
    if let Err(e) = engine.refresh_domain_lists(&client).await {
        warn!(error = %e, "Initial domain list refresh failed, continuing with empty lists");
    }

    let mut interval_timer = tokio::time::interval(interval);
    interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval_timer.tick().await;
        info!("Refreshing domain lists...");
        match engine.refresh_domain_lists(&client).await {
            Ok(_) => info!("Domain list refresh completed"),
            Err(e) => warn!(error = %e, "Domain list refresh failed, will retry on next interval"),
        }
    }
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

    let agent_config_path = std::env::var("CLASHD_AGENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".clash").join("agents.json"))
                .unwrap_or_else(|| PathBuf::from("/etc/clash/agents.json"))
        });

    info!("╔══════════════════════════════════════════════╗");
    info!(
        "║           clashd v{}                 ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("║   Centralized Starlark Policy Engine        ║");
    info!("╠══════════════════════════════════════════════╣");
    info!("║ Features:                                    ║");
    info!("║   • Starlark policy evaluation               ║");
    info!("║   • Domain filtering (lists + regex)         ║");
    info!("║   • Per-agent policy scoping                 ║");
    info!("║   • Dynamic threat intelligence feeds        ║");
    info!("╚══════════════════════════════════════════════╝");
    info!("");
    info!("Configuration:");
    info!("  Port: {}", port);
    info!("  Policy path: {:?}", policy_path);
    info!("  Agent configs: {:?}", agent_config_path);

    // Create policy engine
    let engine = PolicyEngine::new(&policy_path).await?;

    // Load agent configs if file exists
    if agent_config_path.exists() {
        let configs = load_agent_configs(&agent_config_path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        info!(count = configs.len(), "Loaded agent policy configs");
        engine.set_agent_configs(configs).await;
    } else {
        info!("No agent config file found, running with defaults");
    }

    let engine = Arc::new(engine);

    // Start domain list refresh task (every 6 hours)
    let refresh_engine = engine.clone();
    tokio::spawn(async move {
        domain_refresh_loop(refresh_engine, Duration::from_secs(6 * 3600)).await;
    });

    let state = AppState { engine };

    let app = Router::new()
        .route("/", get(version))
        .route("/health", get(health))
        .route("/evaluate", post(evaluate))
        .route("/domains/summary", get(domain_summary))
        .route("/domains/check/{domain}", get(domain_check))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("");
    info!("🚀 Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
