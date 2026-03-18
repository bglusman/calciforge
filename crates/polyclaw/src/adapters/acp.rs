//! AcpAdapter — dispatches to ACP-compliant coding agents via SACP.
//!
//! Connects to agents that implement the Agent Client Protocol (ACP), such as
//! Claude Code (`claude --acp`), OpenCode (`opencode acp`), and others.
//!
//! Unlike the CLI adapter which spawns a fresh subprocess per message, the ACP
//! adapter maintains a persistent agent process with session state across
//! dispatches. Responses are streamed via SACP notifications and collected into
//! a single response string.
//!
//! # Protocol
//!
//! 1. On first dispatch: spawn agent process, initialize ACP, create session
//! 2. On each dispatch: send `PromptRequest`, collect `AgentMessageChunk` notifications
//! 3. Agent process stays alive between dispatches for session continuity
//!
//! # Example config
//!
//! ```toml
//! [[agents]]
//! id = "claude-code"
//! kind = "acp"
//! command = "claude"
//! args = ["--acp"]
//! timeout_ms = 300000
//! model = "claude-sonnet-4-5"
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sacp::schema::{
    ContentBlock, EnvVariable, InitializeRequest, McpServer, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification, SessionUpdate, TextContent, ToolCallStatus,
    VERSION as PROTOCOL_VERSION,
};
use sacp::{ByteStreams, Component, JrConnectionCx};
use sacp::role::ClientToAgent;
use sacp_tokio::AcpAgent;
use tokio::sync::{mpsc, Mutex};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, error, info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT_MS: u64 = 300_000; // 5 minutes — coding agents are slow

/// Internal command sent to the ACP session task.
enum SessionCommand {
    /// Send a prompt and return the collected response.
    Prompt {
        text: String,
        response_tx: tokio::sync::oneshot::Sender<Result<String, AdapterError>>,
    },
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// ACP adapter — connects to an ACP-compliant agent process.
///
/// The agent process is spawned lazily on first dispatch and kept alive for
/// session continuity. All communication uses the SACP protocol over stdio.
pub struct AcpAdapter {
    command: String,
    args: Vec<String>,
    env: std::collections::HashMap<String, String>,
    model: Option<String>,
    model_flag: Option<String>,
    timeout: Duration,
    /// Channel to send commands to the background session task.
    /// `None` until the session is initialized.
    session_tx: Mutex<Option<mpsc::Sender<SessionCommand>>>,
    /// Guards against concurrent initialization.
    init_lock: Mutex<()>,
}

impl AcpAdapter {
    /// Create a new ACP adapter.
    ///
    /// - `command` — path to the agent binary (e.g. `claude`, `opencode`)
    /// - `args` — base arguments (e.g. `["--acp"]`); model flag appended if set
    /// - `env` — additional environment variables
    /// - `model` — model to use (appended via `model_flag`)
    /// - `timeout_ms` — per-prompt timeout (`None` → 300s)
    pub fn new(
        command: String,
        args: Option<Vec<String>>,
        env: std::collections::HashMap<String, String>,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let args = args.unwrap_or_default();

        // Detect model flag from args pattern or default to --model
        let model_flag = if model.is_some() {
            Some("--model".to_string())
        } else {
            None
        };

        Self {
            command,
            args,
            env,
            model,
            model_flag,
            timeout,
            session_tx: Mutex::new(None),
            init_lock: Mutex::new(()),
        }
    }

    /// Build the full args list including model flag if applicable.
    fn full_args(&self) -> Vec<String> {
        let mut args = self.args.clone();
        if let (Some(flag), Some(model)) = (&self.model_flag, &self.model) {
            args.push(flag.clone());
            args.push(model.clone());
        }
        args
    }

    /// Ensure the ACP session is initialized, spawning the agent if needed.
    async fn ensure_session(&self) -> Result<mpsc::Sender<SessionCommand>, AdapterError> {
        // Fast path: session already exists
        {
            let guard = self.session_tx.lock().await;
            if let Some(tx) = guard.as_ref() {
                if !tx.is_closed() {
                    return Ok(tx.clone());
                }
            }
        }

        // Slow path: initialize under lock
        let _init_guard = self.init_lock.lock().await;

        // Double-check after acquiring init lock
        {
            let guard = self.session_tx.lock().await;
            if let Some(tx) = guard.as_ref() {
                if !tx.is_closed() {
                    return Ok(tx.clone());
                }
            }
        }

        info!(
            command = %self.command,
            args = ?self.full_args(),
            "acp: spawning agent process"
        );

        let (session_tx, session_rx) = mpsc::channel::<SessionCommand>(32);

        // Build the AcpAgent
        let server = McpServer::Stdio {
            name: self.command.clone(),
            command: PathBuf::from(&self.command),
            args: self.full_args(),
            env: self
                .env
                .iter()
                .map(|(k, v)| EnvVariable {
                    name: k.clone(),
                    value: v.clone(),
                    meta: None,
                })
                .collect(),
        };
        let agent = AcpAgent::new(server);

        // Spawn the agent process
        let (agent_stdin, agent_stdout, _stderr, mut child) = agent
            .spawn_process()
            .map_err(|e| AdapterError::Unavailable(format!("failed to spawn {}: {}", self.command, e)))?;

        let transport = ByteStreams::new(agent_stdin.compat_write(), agent_stdout.compat());
        let command_name = self.command.clone();

        // Spawn background task that owns the ACP connection
        tokio::spawn(async move {
            let result = run_acp_client_session(transport, session_rx, &command_name).await;
            if let Err(e) = &result {
                error!(command = %command_name, error = %e, "acp session task exited with error");
            }
            // Clean up child process
            let _ = child.kill().await;
            result
        });

        // Store the sender
        {
            let mut guard = self.session_tx.lock().await;
            *guard = Some(session_tx.clone());
        }

        Ok(session_tx)
    }
}

#[async_trait]
impl AgentAdapter for AcpAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let session_tx = self.ensure_session().await?;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        session_tx
            .send(SessionCommand::Prompt {
                text: msg.to_string(),
                response_tx,
            })
            .await
            .map_err(|_| AdapterError::Unavailable("acp session task died".to_string()))?;

        // Wait for response with timeout
        match tokio::time::timeout(self.timeout, response_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(AdapterError::Unavailable(
                "acp session task dropped response channel".to_string(),
            )),
            Err(_) => Err(AdapterError::Timeout),
        }
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        // ACP agents manage their own context, so we just forward the message.
        // The context preamble from polyclaw's ring buffer is still useful for
        // cross-agent context bridging.
        self.dispatch(ctx.message).await
    }

    fn kind(&self) -> &'static str {
        "acp"
    }
}

// ---------------------------------------------------------------------------
// Background session task (transport-generic)
// ---------------------------------------------------------------------------

/// Runs the ACP client session over any SACP transport.
///
/// This is the core protocol logic, separated from process management so it
/// can be tested with in-process channels.
///
/// Initializes the agent, creates a session, then loops processing prompts
/// from `command_rx`, collecting streamed responses from notifications.
async fn run_acp_client_session(
    transport: impl Component + 'static,
    command_rx: mpsc::Receiver<SessionCommand>,
    label: &str,
) -> Result<(), String> {
    // Shared state for collecting response text from notifications
    let response_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let response_buf_clone = response_buf.clone();

    // Channel to forward commands into the with_client closure
    let (inner_tx, mut inner_rx) = mpsc::channel::<SessionCommand>(32);

    // Spawn a forwarding task from command_rx to inner_tx
    let mut command_rx = command_rx;
    let forward_tx = inner_tx.clone();
    tokio::spawn(async move {
        while let Some(cmd) = command_rx.recv().await {
            if forward_tx.send(cmd).await.is_err() {
                break;
            }
        }
    });

    let result = ClientToAgent::builder()
        .name("polyclaw-acp")
        .on_receive_notification(move |notification: SessionNotification, _cx| {
            let buf = response_buf_clone.clone();
            async move {
                match &notification.update {
                    SessionUpdate::AgentMessageChunk(chunk) => {
                        // ContentChunk wraps a ContentBlock
                        if let ContentBlock::Text(text) = &chunk.content {
                            let mut guard = buf.lock().await;
                            guard.push_str(&text.text);
                        }
                    }
                    SessionUpdate::ToolCall(tc) => {
                        debug!(tool = %tc.title, "acp: agent tool call");
                    }
                    SessionUpdate::ToolCallUpdate(update) => {
                        if let Some(status) = &update.fields.status {
                            if *status == ToolCallStatus::Failed {
                                warn!(tool_id = %update.id, "acp: tool call failed");
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
        })
        .on_receive_request(
            |request: RequestPermissionRequest,
             request_cx: sacp::JrRequestCx<RequestPermissionResponse>,
             _cx: JrConnectionCx<ClientToAgent>| async move {
                // Auto-approve tool permissions (polyclaw trusts its agents;
                // Outpost/Clash handle security at the boundary)
                let option_id = request.options.first().map(|opt| opt.id.clone());
                match option_id {
                    Some(id) => {
                        debug!(option = %id, "acp: auto-approving permission");
                        request_cx.respond(RequestPermissionResponse {
                            outcome: RequestPermissionOutcome::Selected { option_id: id },
                            meta: None,
                        })
                    }
                    None => request_cx.respond(RequestPermissionResponse {
                        outcome: RequestPermissionOutcome::Cancelled,
                        meta: None,
                    }),
                }
            },
        )
        .with_client(transport, |cx: JrConnectionCx<ClientToAgent>| {
            let response_buf = response_buf.clone();
            async move {
                // Initialize the agent
                info!("acp: initializing agent");
                let init_response = cx
                    .send_request(InitializeRequest {
                        protocol_version: PROTOCOL_VERSION,
                        client_capabilities: Default::default(),
                        client_info: Default::default(),
                        meta: None,
                    })
                    .block_task()
                    .await?;

                let agent_name = init_response
                    .agent_info
                    .as_ref()
                    .map(|i| i.name.as_str())
                    .unwrap_or("unknown");
                info!(agent = %agent_name, "acp: agent initialized");

                // Create session
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                let session_response = cx
                    .send_request(NewSessionRequest {
                        mcp_servers: vec![],
                        cwd,
                        meta: None,
                    })
                    .block_task()
                    .await?;

                let session_id = session_response.session_id;
                info!(session = %session_id, "acp: session created");

                // Process commands
                while let Some(cmd) = inner_rx.recv().await {
                    match cmd {
                        SessionCommand::Prompt { text, response_tx } => {
                            // Clear the response buffer
                            {
                                let mut guard = response_buf.lock().await;
                                guard.clear();
                            }

                            debug!(prompt_len = text.len(), "acp: sending prompt");

                            // Send the prompt
                            let prompt_result = cx
                                .send_request(PromptRequest {
                                    session_id: session_id.clone(),
                                    prompt: vec![ContentBlock::Text(TextContent {
                                        text,
                                        annotations: None,
                                        meta: None,
                                    })],
                                    meta: None,
                                })
                                .block_task()
                                .await;

                            match prompt_result {
                                Ok(_) => {
                                    // Collect the response
                                    let response = {
                                        let guard = response_buf.lock().await;
                                        guard.trim().to_string()
                                    };

                                    if response.is_empty() {
                                        let _ = response_tx.send(Err(AdapterError::Protocol(
                                            "acp agent returned empty response".to_string(),
                                        )));
                                    } else {
                                        info!(response_len = response.len(), "acp: response collected");
                                        let _ = response_tx.send(Ok(response));
                                    }
                                }
                                Err(e) => {
                                    let _ = response_tx.send(Err(AdapterError::Protocol(
                                        format!("acp prompt error: {}", e),
                                    )));
                                }
                            }
                        }
                    }
                }

                Ok(())
            }
        })
        .await;

    result.map_err(|e| format!("{}: acp session error: {}", label, e))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_kind_is_acp() {
        let adapter = AcpAdapter::new(
            "claude".to_string(),
            Some(vec!["--acp".to_string()]),
            HashMap::new(),
            None,
            None,
        );
        assert_eq!(adapter.kind(), "acp");
    }

    #[test]
    fn test_full_args_without_model() {
        let adapter = AcpAdapter::new(
            "claude".to_string(),
            Some(vec!["--acp".to_string()]),
            HashMap::new(),
            None,
            None,
        );
        assert_eq!(adapter.full_args(), vec!["--acp"]);
    }

    #[test]
    fn test_full_args_with_model() {
        let adapter = AcpAdapter::new(
            "claude".to_string(),
            Some(vec!["--acp".to_string()]),
            HashMap::new(),
            Some("claude-sonnet-4-5".to_string()),
            None,
        );
        assert_eq!(
            adapter.full_args(),
            vec!["--acp", "--model", "claude-sonnet-4-5"]
        );
    }

    #[test]
    fn test_default_timeout_is_5_minutes() {
        let adapter = AcpAdapter::new(
            "claude".to_string(),
            None,
            HashMap::new(),
            None,
            None,
        );
        assert_eq!(adapter.timeout, Duration::from_millis(300_000));
    }

    #[test]
    fn test_custom_timeout() {
        let adapter = AcpAdapter::new(
            "claude".to_string(),
            None,
            HashMap::new(),
            None,
            Some(60_000),
        );
        assert_eq!(adapter.timeout, Duration::from_millis(60_000));
    }

    #[test]
    fn test_default_args_when_none() {
        let adapter = AcpAdapter::new(
            "opencode".to_string(),
            None,
            HashMap::new(),
            None,
            None,
        );
        assert!(adapter.args.is_empty());
    }

    // -----------------------------------------------------------------------
    // In-process mock agent tests using SACP Channel::duplex()
    // -----------------------------------------------------------------------

    use sacp::Channel;
    use sacp::role::AgentToClient;
    use sacp::schema::{
        ContentChunk, Implementation, InitializeResponse, NewSessionResponse,
        PromptResponse, SessionId, StopReason,
    };

    /// Spawn a mock ACP agent that echoes prompts back as responses.
    ///
    /// The mock handles Initialize, NewSession, and Prompt requests.
    /// For each Prompt, it sends an AgentMessageChunk notification with
    /// "echo: {prompt_text}" then returns PromptResponse.
    fn spawn_echo_agent(agent_channel: Channel) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let result = AgentToClient::builder()
                .name("mock-echo-agent")
                .on_receive_request(
                    |_req: InitializeRequest,
                     req_cx: sacp::JrRequestCx<InitializeResponse>,
                     _cx: JrConnectionCx<AgentToClient>| async move {
                        req_cx.respond(InitializeResponse {
                            protocol_version: PROTOCOL_VERSION,
                            agent_info: Some(Implementation {
                                name: "MockEchoAgent".to_string(),
                                title: None,
                                version: "0.1.0".to_string(),
                            }),
                            agent_capabilities: Default::default(),
                            auth_methods: vec![],
                            meta: None,
                        })
                    },
                )
                .on_receive_request(
                    |_req: NewSessionRequest,
                     req_cx: sacp::JrRequestCx<NewSessionResponse>,
                     _cx: JrConnectionCx<AgentToClient>| async move {
                        req_cx.respond(NewSessionResponse {
                            session_id: SessionId(std::sync::Arc::from("test-session-1")),
                            modes: None,
                            meta: None,
                        })
                    },
                )
                .on_receive_request(
                    |req: PromptRequest,
                     req_cx: sacp::JrRequestCx<PromptResponse>,
                     cx: JrConnectionCx<AgentToClient>| async move {
                        // Extract prompt text
                        let prompt_text = req
                            .prompt
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");

                        // Send response as notification chunk (like a real agent)
                        let response_text = format!("echo: {}", prompt_text);
                        cx.send_notification(SessionNotification {
                            session_id: req.session_id.clone(),
                            update: SessionUpdate::AgentMessageChunk(ContentChunk {
                                content: ContentBlock::Text(TextContent {
                                    text: response_text,
                                    annotations: None,
                                    meta: None,
                                }),
                                meta: None,
                            }),
                            meta: None,
                        });

                        req_cx.respond(PromptResponse {
                            stop_reason: StopReason::EndTurn,
                            meta: None,
                        })
                    },
                )
                .serve(agent_channel)
                .await;

            if let Err(e) = result {
                eprintln!("mock agent error: {}", e);
            }
        })
    }

    /// Helper: create a session channel connected to a mock echo agent,
    /// and return the command sender for dispatching prompts.
    async fn setup_mock_session() -> (
        mpsc::Sender<SessionCommand>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_channel, agent_channel) = Channel::duplex();

        // Spawn the mock agent
        let agent_handle = spawn_echo_agent(agent_channel);

        // Create the session command channel
        let (session_tx, session_rx) = mpsc::channel::<SessionCommand>(32);

        // Run the client session in a background task
        tokio::spawn(async move {
            let _ = run_acp_client_session(client_channel, session_rx, "test").await;
        });

        // Give the session time to initialize
        tokio::time::sleep(Duration::from_millis(200)).await;

        (session_tx, agent_handle)
    }

    #[tokio::test]
    async fn test_session_echo_prompt() {
        let (session_tx, _agent) = setup_mock_session().await;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        session_tx
            .send(SessionCommand::Prompt {
                text: "hello world".to_string(),
                response_tx,
            })
            .await
            .expect("send should succeed");

        let result = tokio::time::timeout(Duration::from_secs(5), response_rx)
            .await
            .expect("should not timeout")
            .expect("channel should not drop");

        let response = result.expect("should get OK response");
        assert_eq!(response, "echo: hello world");
    }

    #[tokio::test]
    async fn test_session_multiple_prompts() {
        let (session_tx, _agent) = setup_mock_session().await;

        for i in 0..3 {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            session_tx
                .send(SessionCommand::Prompt {
                    text: format!("message {}", i),
                    response_tx,
                })
                .await
                .expect("send should succeed");

            let result = tokio::time::timeout(Duration::from_secs(5), response_rx)
                .await
                .expect("should not timeout")
                .expect("channel should not drop");

            let response = result.expect("should get OK response");
            assert_eq!(response, format!("echo: message {}", i));
        }
    }

    #[tokio::test]
    async fn test_closed_session_channel_returns_send_error() {
        // Create a channel and immediately close the receiver side
        let (session_tx, session_rx) = mpsc::channel::<SessionCommand>(1);
        drop(session_rx);

        let (response_tx, _response_rx) =
            tokio::sync::oneshot::channel::<Result<String, AdapterError>>();

        let send_result = session_tx
            .send(SessionCommand::Prompt {
                text: "hello".to_string(),
                response_tx,
            })
            .await;

        assert!(send_result.is_err(), "send should fail when receiver is dropped");
    }

    #[tokio::test]
    async fn test_dispatch_nonexistent_binary_returns_unavailable() {
        let adapter = AcpAdapter::new(
            "/usr/local/bin/does-not-exist-xyzzy-acp".to_string(),
            Some(vec!["--acp".to_string()]),
            HashMap::new(),
            None,
            Some(2000),
        );
        let result = adapter.dispatch("hello").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(msg) => {
                assert!(msg.contains("does-not-exist"), "got: {}", msg);
            }
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }
}
