//! ACP (Agent Communication Protocol) Adapter for PolyClaw
//!
//! Bridges PolyClaw to ACP-compliant agents (Claude Code, Codex, etc.)
//! via stdio, HTTP, or Unix socket transports.
//!
//! ACP Spec: https://github.com/i-am-bee/acp

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info, warn};

use crate::adapters::{AdapterError, AgentAdapter, DispatchContext};
use crate::config::AgentConfig;

/// ACP adapter implementing stdio-based ACP agent communication.
pub struct AcpAdapter {
    config: AgentConfig,
    agent_id: String,
}

impl AcpAdapter {
    /// Create a new ACP adapter from AgentConfig.
    pub fn new(agent_id: String, config: AgentConfig) -> Result<Self, AdapterError> {
        // Validate that we have a command for stdio transport
        if config.command.is_none() {
            return Err(AdapterError::Unavailable(
                "ACP adapter requires 'command' field".to_string(),
            ));
        }

        Ok(Self {
            config,
            agent_id,
        })
    }

    /// Spawn the ACP agent process and return stdin/stdout handles.
    async fn spawn_agent(&self) -> Result<(tokio::process::ChildStdin, tokio::process::ChildStdout), AdapterError> {
        let command = self.config.command.as_ref().unwrap();
        let args = self.config.args.clone().unwrap_or_default();

        info!(command = %command, args = ?args, "Spawning ACP agent");

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()); // ACP agents log to stderr, ignore for now

        // Set environment variables if provided
        if let Some(env_vars) = &self.config.env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        let mut child: Child = cmd.spawn().map_err(|e| {
            AdapterError::Unavailable(format!("Failed to spawn ACP agent: {}", e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AdapterError::Protocol("Failed to get stdin".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AdapterError::Protocol("Failed to get stdout".to_string()))?;

        // Spawn a task to wait for the child process
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok((stdin, stdout))
    }

    /// Send a message to the ACP agent and read the response.
    async fn acp_request(
        &self,
        stdin: &mut tokio::process::ChildStdin,
        stdout: &mut tokio::process::ChildStdout,
        message: &str,
    ) -> Result<String, AdapterError> {
        // Build ACP JSON-RPC request
        let request = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "prompt".to_string(),
            params: AcpParams {
                prompt: message.to_string(),
            },
        };

        let request_json = serde_json::to_string(&request).map_err(|e| {
            AdapterError::Protocol(format!("Failed to serialize ACP request: {}", e))
        })?;

        debug!(request = %request_json, "Sending ACP request");

        // Send request with newline delimiter
        stdin
            .write_all(format!("{}\n", request_json).as_bytes())
            .await
            .map_err(|e| AdapterError::Unavailable(format!("Failed to write to agent: {}", e)))?;

        stdin.flush().await.map_err(|e| {
            AdapterError::Unavailable(format!("Failed to flush stdin: {}", e))
        })?;

        // Read response line
        let mut reader = BufReader::new(stdout).lines();
        let line = reader
            .next_line()
            .await
            .map_err(|e| AdapterError::Unavailable(format!("Failed to read from agent: {}", e)))?
            .ok_or_else(|| AdapterError::Protocol("Agent closed connection".to_string()))?;

        debug!(response = %line, "Received ACP response");

        // Parse ACP response
        let response: AcpResponse = serde_json::from_str(&line).map_err(|e| {
            AdapterError::Protocol(format!("Failed to parse ACP response: {}", e))
        })?;

        // Check for error
        if let Some(error) = response.error {
            return Err(AdapterError::Protocol(format!(
                "ACP error {}: {}",
                error.code, error.message
            )));
        }

        // Extract result
        response
            .result
            .map(|r| r.response)
            .ok_or_else(|| AdapterError::Protocol("Empty ACP result".to_string()))
    }
}

#[async_trait]
impl AgentAdapter for AcpAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg)).await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let (mut stdin, mut stdout) = self.spawn_agent().await?;

        // Include sender context if available
        let message = if let Some(sender) = ctx.sender {
            format!("[From: {}] {}", sender, ctx.message)
        } else {
            ctx.message.to_string()
        };

        // Send request and get response
        let response = self
            .acp_request(&mut stdin, &mut stdout, &message)
            .await?;

        Ok(response)
    }

    fn kind(&self) -> &'static str {
        "acp"
    }
}

// ---------------------------------------------------------------------------
// ACP JSON-RPC Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AcpRequest {
    jsonrpc: String,
    id: u32,
    method: String,
    params: AcpParams,
}

#[derive(Debug, Serialize)]
struct AcpParams {
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct AcpResponse {
    jsonrpc: String,
    id: Option<u32>,
    #[serde(default)]
    result: Option<AcpResult>,
    #[serde(default)]
    error: Option<AcpError>,
}

#[derive(Debug, Deserialize)]
struct AcpResult {
    response: String,
}

#[derive(Debug, Deserialize)]
struct AcpError {
    code: i32,
    message: String,
}
