//! ACP (Agent Communication Protocol) Adapter for PolyClaw
//!
//! This adapter leverages existing crates from the ACP ecosystem:
//! - `acpx`: Thin client for stdio-based ACP agent connections
//! - `agent-client-protocol`: Official ACP protocol types
//! - `sacp`: Symposium's ACP SDK for middleware/proxy chains (optional)
//!
//! ACP Specification: https://github.com/i-am-bee/acp

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::adapters::{Adapter, AdapterConfig, AdapterError, AdapterFactory, Message};
use crate::config::AcpAgentConfig;

// Re-export ACP types from official crate
pub use agent_client_protocol as acp;

/// Unique identifier for ACP sessions
pub type SessionId = String;

/// Session state for an ACP connection
#[derive(Debug, Clone)]
pub struct AcpSession {
    pub id: SessionId,
    pub agent_id: String,
    pub user_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
    /// ACP session handle from acpx
    pub acp_session: Option<acpx::Connection>,
}

/// ACP Adapter implementation using `acpx` for connections
/// 
/// This adapter uses the existing `acpx` crate (a thin client for ACP) instead
/// of implementing the protocol from scratch. For middleware/proxy chains,
/// the optional `sacp` feature enables SACP (Symposium's ACP extensions).
pub struct AcpAdapter {
    config: AcpAgentConfig,
    agent_id: String,
    /// acpx runtime context for spawning agents
    runtime: acpx::RuntimeContext,
    /// Cached agent server definition
    agent_server: Option<acpx::CommandAgentServer>,
    /// Active sessions
    sessions: Arc<RwLock<HashMap<SessionId, AcpSession>>>,
}

impl AcpAdapter {
    /// Create a new ACP adapter with the given configuration
    pub fn new(agent_id: String, config: AcpAgentConfig) -> Result<Self, AdapterError> {
        // Create acpx runtime context
        let runtime = acpx::RuntimeContext::new(|task| {
            tokio::runtime::Handle::current().block_on(task);
        });

        // Build agent server definition from config
        let agent_server = Self::build_agent_server(&config)?;

        Ok(Self {
            config,
            agent_id,
            runtime,
            agent_server: Some(agent_server),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Build an acpx AgentServer from configuration
    fn build_agent_server(config: &AcpAgentConfig) -> Result<acpx::CommandAgentServer, AdapterError> {
        use acpx::{AgentServerMetadata, CommandAgentServer, CommandSpec};

        let command = config.command.as_ref().ok_or_else(|| {
            AdapterError::Configuration("Missing 'command' for ACP agent".to_string())
        })?;

        let metadata = AgentServerMetadata::new(
            &config.agent_name().unwrap_or_else(|| "acp-agent".to_string()),
            &config.display_name().unwrap_or_else(|| "ACP Agent".to_string()),
            &config.version().unwrap_or_else(|| "0.1.0".to_string()),
        );

        let mut cmd_spec = CommandSpec::new(command);
        
        if let Some(args) = &config.args {
            for arg in args {
                cmd_spec = cmd_spec.arg(arg);
            }
        }

        // Note: acpx handles working_dir and env via the spawned process
        // We don't need to manually set them here

        Ok(CommandAgentServer::new(metadata, cmd_spec))
    }

    /// Get or create an acpx connection for this agent
    async fn get_connection(&self) -> Result<acpx::Connection, AdapterError> {
        let server = self.agent_server.as_ref().ok_or_else(|| {
            AdapterError::Configuration("Agent server not configured".to_string())
        })?;

        server.connect(&self.runtime)
            .await
            .map_err(|e| AdapterError::Connection(format!("acpx connect failed: {}", e)))
    }

    /// Create a new session for a PolyClaw user
    async fn create_session(&self, user_id: &str) -> Result<SessionId, AdapterError> {
        let session_id = format!("acp_{}_{}", self.agent_id, uuid::Uuid::new_v4());
        
        // Establish ACP connection via acpx
        let connection = self.get_connection().await?;

        // Initialize the ACP connection
        let init_request = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_info(acp::Implementation::new("polyclaw", env!("CARGO_PKG_VERSION"))
                .title("PolyClaw Gateway"));

        connection.initialize(init_request)
            .await
            .map_err(|e| AdapterError::Connection(format!("ACP init failed: {}", e)))?;

        let session = AcpSession {
            id: session_id.clone(),
            agent_id: self.agent_id.clone(),
            user_id: user_id.to_string(),
            created_at: chrono::Utc::now(),
            last_activity: chrono::Utc::now(),
            acp_session: Some(connection),
        };

        self.sessions.write().await.insert(session_id.clone(), session);
        
        info!("ACP session created: {} for user {}", session_id, user_id);
        Ok(session_id)
    }

    /// Get existing session or create new one
    async fn get_or_create_session(&self, user_id: &str) -> Result<SessionId, AdapterError> {
        let sessions = self.sessions.read().await;
        
        // Find existing session for user
        for (id, session) in sessions.iter() {
            if session.user_id == user_id {
                return Ok(id.clone());
            }
        }
        
        drop(sessions);
        self.create_session(user_id).await
    }

    /// Convert PolyClaw message to ACP format
    fn polyclaw_to_acp_request(&self, msg: &Message) -> acp::PromptRequest {
        acp::PromptRequest::new(&msg.content)
            .with_context(acp::Context::new()
                .with_meta("polyclaw_sender_id", msg.sender_id.clone())
                .with_meta("polyclaw_channel", msg.channel.clone())
                .with_meta("polyclaw_thread_id", msg.thread_id.clone().unwrap_or_default()))
    }

    /// Convert ACP response to PolyClaw format
    fn acp_to_polyclaw(&self, content: String, session_id: &str) -> Message {
        Message {
            content,
            sender_id: self.agent_id.clone(),
            channel: "acp".to_string(),
            thread_id: Some(session_id.to_string()),
            timestamp: chrono::Utc::now(),
        }
    }
}

#[async_trait]
impl Adapter for AcpAdapter {
    async fn send(&self, msg: Message) -> Result<Message, AdapterError> {
        let session_id = self.get_or_create_session(&msg.sender_id).await?;
        
        // Get the connection from session
        let sessions = self.sessions.read().await;
        let session = sessions.get(&session_id)
            .ok_or_else(|| AdapterError::Connection("Session not found".to_string()))?;
        
        let connection = session.acp_session.as_ref()
            .ok_or_else(|| AdapterError::Connection("No ACP connection".to_string()))?;

        // Convert and send the prompt
        let prompt_req = self.polyclaw_to_acp_request(&msg);
        
        let prompt_result = connection.prompt(prompt_req)
            .await
            .map_err(|e| AdapterError::Agent(format!("ACP prompt failed: {}", e)))?;

        // Update last activity
        drop(sessions);
        if let Some(session) = self.sessions.write().await.get_mut(&session_id) {
            session.last_activity = chrono::Utc::now();
        }

        // Extract response content
        let content = prompt_result.content;
        
        Ok(self.acp_to_polyclaw(content, &session_id))
    }

    async fn send_streaming(
        &self,
        msg: Message,
    ) -> Result<mpsc::UnboundedReceiver<Message>, AdapterError> {
        let session_id = self.get_or_create_session(&msg.sender_id).await?;
        let (tx, rx) = mpsc::unbounded_channel();

        // Get connection
        let sessions = self.sessions.read().await;
        let session = sessions.get(&session_id)
            .ok_or_else(|| AdapterError::Connection("Session not found".to_string()))?;
        
        let connection = session.acp_session.as_ref()
            .ok_or_else(|| AdapterError::Connection("No ACP connection".to_string()))?;

        let prompt_req = self.polyclaw_to_acp_request(&msg);
        let adapter_id = self.agent_id.clone();
        let session_id_clone = session_id.clone();

        // Spawn streaming handler
        tokio::spawn(async move {
            match connection.prompt_streaming(prompt_req).await {
                Ok(mut stream) => {
                    while let Some(chunk) = stream.next().await {
                        let message = Message {
                            content: chunk.content,
                            sender_id: adapter_id.clone(),
                            channel: "acp".to_string(),
                            thread_id: Some(session_id_clone.clone()),
                            timestamp: chrono::Utc::now(),
                        };
                        
                        if tx.send(message).is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Streaming error: {}", e);
                }
            }
        });

        Ok(rx)
    }

    async fn health_check(&self) -> Result<(), AdapterError> {
        // Try to establish a connection
        let _connection = self.get_connection().await?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        // Close all sessions
        let sessions = self.sessions.write().await;
        for (session_id, session) in sessions.iter() {
            if let Some(conn) = &session.acp_session {
                if let Err(e) = conn.close().await {
                    warn!("Failed to close session {}: {}", session_id, e);
                }
            }
        }
        
        info!("ACP adapter {}: shutdown complete", self.agent_id);
        Ok(())
    }

    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn kind(&self) -> &str {
        "acp"
    }
}

/// Factory for creating ACP adapters
pub struct AcpAdapterFactory;

impl AcpAdapterFactory {
    pub fn new() -> Self {
        Self
    }
}

impl AdapterFactory for AcpAdapterFactory {
    fn create(&self, agent_id: String, config: &AdapterConfig) -> Result<Box<dyn Adapter>, AdapterError> {
        match config {
            AdapterConfig::Acp(acp_config) => {
                Ok(Box::new(AcpAdapter::new(agent_id, acp_config.clone())?))
            }
            _ => Err(AdapterError::Configuration(
                "Expected ACP config".to_string()
            )),
        }
    }

    fn kind(&self) -> &str {
        "acp"
    }
}

impl Default for AcpAdapterFactory {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SACP Middleware Support (Optional Feature)
// ============================================================================

#[cfg(feature = "acp-middleware")]
pub mod middleware {
    //! SACP (Symposium's ACP extensions) middleware support
    //!
    //! This module provides integration with `sacp` for building composable
    //! middleware chains that can transform, log, or extend ACP messages.
    //!
    //! Example middleware chain:
    //! ```text
    //! PolyClaw -> AuthProxy -> LoggingProxy -> MCPProxy -> Base ACP Agent
    //! ```

    use super::*;
    use sacp::{Client, Proxy, Builder};

    /// Middleware trait for ACP message transformation
    #[async_trait]
    pub trait AcpMiddleware: Send + Sync {
        /// Transform an outgoing request
        async fn transform_request(&self, req: acp::PromptRequest) -> acp::PromptRequest {
            req
        }

        /// Transform an incoming response
        async fn transform_response(&self, resp: String) -> String {
            resp
        }
    }

    /// Middleware chain wrapper for ACP adapter
    pub struct MiddlewareAcpAdapter {
        base: AcpAdapter,
        middleware: Vec<Box<dyn AcpMiddleware>>,
    }

    impl MiddlewareAcpAdapter {
        /// Add middleware to the chain
        pub fn with_middleware(mut self, mw: Box<dyn AcpMiddleware>) -> Self {
            self.middleware.push(mw);
            self
        }
    }

    #[async_trait]
    impl Adapter for MiddlewareAcpAdapter {
        async fn send(&self, msg: Message) -> Result<Message, AdapterError> {
            // Apply request middleware
            let mut req = self.base.polyclaw_to_acp_request(&msg);
            for mw in &self.middleware {
                req = mw.transform_request(req).await;
            }

            // Send via base adapter
            let result = self.base.send(msg).await?;

            // Apply response middleware
            let mut content = result.content;
            for mw in &self.middleware {
                content = mw.transform_response(content).await;
            }

            Ok(Message {
                content,
                ..result
            })
        }

        async fn health_check(&self) -> Result<(), AdapterError> {
            self.base.health_check().await
        }

        async fn shutdown(&self) -> Result<(), AdapterError> {
            self.base.shutdown().await
        }

        fn agent_id(&self) -> &str {
            self.base.agent_id()
        }

        fn kind(&self) -> &str {
            "acp-middleware"
        }
    }

    /// Example: Authentication middleware that adds auth headers
    pub struct AuthMiddleware {
        token: String,
    }

    #[async_trait]
    impl AcpMiddleware for AuthMiddleware {
        async fn transform_request(&self, mut req: acp::PromptRequest) -> acp::PromptRequest {
            req.with_context(acp::Context::new()
                .with_meta("auth_token", self.token.clone()))
        }
    }

    /// Example: Logging middleware for audit trails
    pub struct LoggingMiddleware {
        prefix: String,
    }

    #[async_trait]
    impl AcpMiddleware for LoggingMiddleware {
        async fn transform_request(&self, req: acp::PromptRequest) -> acp::PromptRequest {
            tracing::info!("{}: Sending prompt: {}", self.prefix, req.content);
            req
        }

        async fn transform_response(&self, resp: String) -> String {
            tracing::info!("{}: Received response ({} bytes)", self.prefix, resp.len());
            resp
        }
    }
}
