//! Mock channel for testing and development.
//!
//! This channel simulates user interactions without external dependencies.
//! It can be controlled via HTTP API to inject test messages and verify responses.
//!
//! Configuration:
//! ```toml
//! [[channels]]
//! kind = "mock"
//! enabled = true
//! ```
//!
//! The control port defaults to 9090 and can be overridden via `control_port` in channel config.
//!
//! Note: the `/send` endpoint is a stub — it logs the message and returns a fixed response.
//! Wire `router`/`command_handler` here when full integration testing is needed.

use anyhow::{Context, Result};
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::sync::Arc;

use crate::{
    commands::CommandHandler, config::PolyConfig, context::ContextStore,
    router::Router as CalciforgeRouter,
};

/// Mock channel state
#[derive(Clone)]
#[allow(dead_code)]
struct MockState {
    /// Messages sent by the mock channel (for verification)
    sent_messages: Arc<Mutex<Vec<MockMessage>>>,
    /// Messages received by the mock channel (from test API)
    received_messages: Arc<Mutex<Vec<MockMessage>>>,
    /// Router for sending to agents
    router: Arc<CalciforgeRouter>,
    /// Command handler
    command_handler: Arc<CommandHandler>,
    /// Context store
    context_store: ContextStore,
    /// Configuration
    config: Arc<PolyConfig>,
}

/// A mock message for logging
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockMessage {
    /// Message ID
    id: String,
    /// Sender ID
    sender: String,
    /// Message text
    text: String,
    /// Timestamp (ISO 8601)
    timestamp: String,
    /// Response (if any)
    response: Option<String>,
}

/// API request to send a test message
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SendMessageRequest {
    /// Sender ID (must be in allowed_users/test_users)
    sender: String,
    /// Message text
    text: String,
    /// Optional: Expected response pattern (for verification)
    expected_response: Option<String>,
}

/// API response
#[derive(Debug, Serialize)]
struct ApiResponse {
    success: bool,
    message: String,
    data: Option<serde_json::Value>,
}

/// Run the mock channel
pub async fn run(
    config: Arc<PolyConfig>,
    router: Arc<CalciforgeRouter>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
) -> Result<()> {
    // Find the mock channel config
    let mock_channel = config
        .channels
        .iter()
        .find(|c| c.kind == "mock" && c.enabled)
        .context("no enabled mock channel found in config")?;

    info!("Starting mock channel");

    // Create state
    let state = MockState {
        sent_messages: Arc::new(Mutex::new(Vec::new())),
        received_messages: Arc::new(Mutex::new(Vec::new())),
        router: router.clone(),
        command_handler: command_handler.clone(),
        context_store: context_store.clone(),
        config: config.clone(),
    };

    let control_port = mock_channel.control_port.unwrap_or(9090);

    // Start control API server
    let control_app = Router::new()
        .route("/health", get(health_handler))
        .route("/messages", get(get_messages_handler))
        .route("/send", post(send_message_handler))
        .route("/clear", post(clear_messages_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", control_port))
        .await
        .context(format!("failed to bind to port {}", control_port))?;

    info!(
        "Mock channel control API listening on port {}",
        control_port
    );
    info!("Use HTTP API to send test messages:");
    info!("  GET  http://127.0.0.1:{}/health", control_port);
    info!("  GET  http://127.0.0.1:{}/messages", control_port);
    info!("  POST http://127.0.0.1:{}/send", control_port);
    info!("  POST http://127.0.0.1:{}/clear", control_port);

    axum::serve(listener, control_app)
        .await
        .context("mock channel control API server failed")?;

    Ok(())
}

/// Health check handler
async fn health_handler() -> impl IntoResponse {
    Json(ApiResponse {
        success: true,
        message: "Mock channel control API is healthy".to_string(),
        data: None,
    })
}

/// Get all messages handler
async fn get_messages_handler(State(state): State<MockState>) -> impl IntoResponse {
    let sent = state.sent_messages.lock().await;
    let received = state.received_messages.lock().await;

    Json(ApiResponse {
        success: true,
        message: format!(
            "{} sent messages, {} received messages",
            sent.len(),
            received.len()
        ),
        data: Some(serde_json::json!({
            "sent": &*sent,
            "received": &*received,
        })),
    })
}

/// Send a test message handler
async fn send_message_handler(
    State(state): State<MockState>,
    Json(req): Json<SendMessageRequest>,
) -> impl IntoResponse {
    let message_id = format!("mock-{}", chrono::Utc::now().timestamp_millis());
    let timestamp = chrono::Utc::now().to_rfc3339();

    // Create mock message
    let mock_message = MockMessage {
        id: message_id.clone(),
        sender: req.sender.clone(),
        text: req.text.clone(),
        timestamp: timestamp.clone(),
        response: None,
    };

    // Store in received messages
    {
        let mut received = state.received_messages.lock().await;
        received.push(mock_message.clone());
    }

    info!(sender = %req.sender, text = %req.text, "Mock channel received message");

    // TODO: Actually route the message through the system
    // For now, just log it
    debug!("Would route message from {}: {}", req.sender, req.text);

    // Simulate a response
    let response_text = format!("Mock response to: {}", req.text);

    // Create response message
    let response_message = MockMessage {
        id: format!("resp-{}", message_id),
        sender: "system".to_string(),
        text: response_text.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        response: None,
    };

    // Store in sent messages
    {
        let mut sent = state.sent_messages.lock().await;
        sent.push(response_message);
    }

    // Update original message with response
    {
        let mut received = state.received_messages.lock().await;
        if let Some(msg) = received.iter_mut().find(|m| m.id == message_id) {
            msg.response = Some(response_text.clone());
        }
    }

    Json(ApiResponse {
        success: true,
        message: "Message sent and response simulated".to_string(),
        data: Some(serde_json::json!({
            "message_id": message_id,
            "response": response_text,
        })),
    })
}

/// Clear all messages handler
async fn clear_messages_handler(State(state): State<MockState>) -> impl IntoResponse {
    {
        let mut sent = state.sent_messages.lock().await;
        sent.clear();

        let mut received = state.received_messages.lock().await;
        received.clear();
    }

    Json(ApiResponse {
        success: true,
        message: "All messages cleared".to_string(),
        data: None,
    })
}
