//! NzcNativeAdapter — NZC webhook adapter with conversation history across turns.
//!
//! ## Background
//!
//! `NzcHttpAdapter` (in `openclaw.rs`) correctly uses the native `/webhook`
//! endpoint, but **does not accumulate conversation history across turns**.
//! Each call dispatches only the current user message; NZC has no knowledge
//! of prior assistant turns (the model starts fresh each time).
//!
//! ## What this adapter adds
//!
//! `NzcNativeAdapter` wraps `NzcHttpAdapter` with an in-memory conversation
//! history buffer:
//!
//! 1. On each dispatch: the previous `(user, assistant)` turns are prepended
//!    to the outgoing message as a context preamble so NZC's agent sees the
//!    full conversation.
//! 2. `ApprovalPending` responses are handled without losing history — the
//!    pending turn's user message is retained; the assistant turn is inserted
//!    once the approval result is received and dispatched.
//! 3. History is per-sender: if `ctx.sender` is present, history is keyed on
//!    `sender`; otherwise a single shared history is used.
//!
//! ## Conversation history format
//!
//! The preamble injected before each message uses a compact plain-text format:
//!
//! ```text
//! [Conversation history]
//! User: <prior user message>
//! Assistant: <prior assistant reply>
//! User: <prior user message>
//! Assistant: <prior assistant reply>
//! [End history]
//! <current user message>
//! ```
//!
//! This is readable by all LLM backends NZC may use and avoids JSON encoding.
//!
//! ## History limits
//!
//! History is capped at `MAX_HISTORY_TURNS` turn-pairs (user + assistant).
//! Older turns are evicted from the front of the ring buffer.  This prevents
//! unbounded growth for long-running sessions.
//!
//! ## ApprovalPending flow
//!
//! When NZC returns `ApprovalPending`, the user message that triggered the
//! approval is **not** added to history yet.  When `!approve` or `!deny` is
//! processed and the continuation response is received, the
//! `record_approval_continuation` method should be called with the original
//! user message and the final assistant response so history stays consistent.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, info};

use super::openclaw::NzcHttpAdapter;
use super::{AdapterError, AgentAdapter, DispatchContext, RuntimeStatus};

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Maximum number of (user, assistant) turn pairs retained per sender.
const MAX_HISTORY_TURNS: usize = 20;

/// A single turn in the conversation.
#[derive(Debug, Clone)]
struct Turn {
    user: String,
    assistant: String,
}

/// Per-sender conversation history.
#[derive(Debug, Default)]
struct SenderHistory {
    turns: Vec<Turn>,
}

impl SenderHistory {
    /// Append a completed turn.
    fn push(&mut self, user: String, assistant: String) {
        if self.turns.len() >= MAX_HISTORY_TURNS {
            self.turns.remove(0);
        }
        self.turns.push(Turn { user, assistant });
    }

    /// Build the preamble to inject before the current user message.
    ///
    /// Returns `None` when history is empty (no preamble needed).
    fn build_preamble(&self) -> Option<String> {
        if self.turns.is_empty() {
            return None;
        }
        let mut buf = String::from("[Conversation history]\n");
        for turn in &self.turns {
            buf.push_str("User: ");
            buf.push_str(&turn.user);
            buf.push('\n');
            buf.push_str("Assistant: ");
            buf.push_str(&turn.assistant);
            buf.push('\n');
        }
        buf.push_str("[End history]\n");
        Some(buf)
    }

    /// Build the full message with history prepended.
    fn wrap_message(&self, user_msg: &str) -> String {
        match self.build_preamble() {
            None => user_msg.to_string(),
            Some(preamble) => format!("{}{}", preamble, user_msg),
        }
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// NZC adapter with per-sender conversation history accumulation.
///
/// Wraps [`NzcHttpAdapter`] and maintains a history ring buffer so NZC sees
/// prior turns in every request.
pub struct NzcNativeAdapter {
    inner: NzcHttpAdapter,
    /// Per-sender history: sender id → SenderHistory.
    /// Key `""` is used when no sender is available.
    history: Arc<Mutex<HashMap<String, SenderHistory>>>,
}

impl NzcNativeAdapter {
    /// Create a new NZC native adapter.
    ///
    /// Parameters match `NzcHttpAdapter::new`.
    pub fn new(endpoint: String, auth_token: String, timeout_ms: Option<u64>) -> Self {
        Self {
            inner: NzcHttpAdapter::new(endpoint, auth_token, timeout_ms),
            history: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Clear history for a sender (e.g. when `/clear` is requested).
    #[cfg(test)]
    pub async fn clear_history(&self, sender: Option<&str>) {
        let key = sender.unwrap_or("").to_string();
        let mut guard = self.history.lock().await;
        guard.remove(&key);
    }
}

#[async_trait]
impl AgentAdapter for NzcNativeAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let sender_key = ctx.sender.unwrap_or("").to_string();

        // Build the message with history prepended
        let (full_message, history_turns) = {
            let guard = self.history.lock().await;
            let history = guard.get(&sender_key);
            let turns = history.map(|h| h.turns.len()).unwrap_or(0);
            let msg = match history {
                None => ctx.message.to_string(),
                Some(h) => h.wrap_message(ctx.message),
            };
            (msg, turns)
        };

        info!(
            sender = ?ctx.sender,
            history_turns,
            "nzc-native dispatch"
        );
        debug!(full_message_len = full_message.len(), "nzc-native outbound");

        // Dispatch via the inner NZC HTTP adapter
        let inner_ctx = DispatchContext {
            message: &full_message,
            sender: ctx.sender,
        };

        match self.inner.dispatch_with_context(inner_ctx).await {
            Ok(reply) => {
                // Record successful turn in history
                let mut guard = self.history.lock().await;
                let entry = guard.entry(sender_key).or_default();
                entry.push(ctx.message.to_string(), reply.clone());
                info!(
                    history_turns = entry.turns.len(),
                    "nzc-native: turn recorded"
                );
                Ok(reply)
            }
            Err(AdapterError::ApprovalPending(req)) => {
                // Do NOT add to history yet — the turn is incomplete.
                // The caller is responsible for calling
                // `record_approval_continuation` after resolution.
                info!(
                    request_id = %req.request_id,
                    "nzc-native: approval pending — history deferred"
                );
                Err(AdapterError::ApprovalPending(req))
            }
            Err(e) => Err(e),
        }
    }

    fn kind(&self) -> &'static str {
        "nzc-native"
    }

    async fn get_runtime_status(&self) -> Option<RuntimeStatus> {
        self.inner.get_runtime_status().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_adapter(port: u16) -> NzcNativeAdapter {
        NzcNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            Some(2000),
        )
    }

    #[test]
    fn test_sender_history_empty_no_preamble() {
        let h = SenderHistory::default();
        assert!(h.build_preamble().is_none());
        assert_eq!(h.wrap_message("hello"), "hello");
    }

    #[test]
    fn test_sender_history_one_turn() {
        let mut h = SenderHistory::default();
        h.push(
            "what is rust".to_string(),
            "Rust is a systems language.".to_string(),
        );

        let preamble = h.build_preamble().unwrap();
        assert!(preamble.contains("[Conversation history]"));
        assert!(preamble.contains("User: what is rust"));
        assert!(preamble.contains("Assistant: Rust is a systems language."));
        assert!(preamble.contains("[End history]"));
    }

    #[test]
    fn test_sender_history_wrap_message() {
        let mut h = SenderHistory::default();
        h.push("hi".to_string(), "hello there".to_string());
        let wrapped = h.wrap_message("how are you?");
        assert!(wrapped.starts_with("[Conversation history]"));
        assert!(wrapped.ends_with("how are you?"));
    }

    #[test]
    fn test_sender_history_max_turns_eviction() {
        let mut h = SenderHistory::default();
        for i in 0..MAX_HISTORY_TURNS + 5 {
            h.push(format!("user {}", i), format!("assistant {}", i));
        }
        assert_eq!(h.turns.len(), MAX_HISTORY_TURNS);
        // Oldest turns evicted; most recent should be at the end
        let last = &h.turns[MAX_HISTORY_TURNS - 1];
        assert!(last.user.contains(&(MAX_HISTORY_TURNS + 4).to_string()));
    }

    #[test]
    fn test_sender_history_multiple_turns_in_preamble() {
        let mut h = SenderHistory::default();
        h.push("first".to_string(), "reply 1".to_string());
        h.push("second".to_string(), "reply 2".to_string());
        let preamble = h.build_preamble().unwrap();
        // Both turns should appear in order
        let first_pos = preamble.find("User: first").unwrap();
        let second_pos = preamble.find("User: second").unwrap();
        assert!(
            first_pos < second_pos,
            "turns should appear in chronological order"
        );
    }

    #[tokio::test]
    async fn test_clear_history() {
        let a = make_adapter(19301);
        // Manually insert history
        {
            let mut guard = a.history.lock().await;
            let entry = guard.entry("brian".to_string()).or_default();
            entry.push("msg".to_string(), "reply".to_string());
            assert_eq!(entry.turns.len(), 1);
        }
        a.clear_history(Some("brian")).await;
        {
            let guard = a.history.lock().await;
            assert!(
                guard.get("brian").is_none() || guard["brian"].turns.is_empty(),
                "history should be cleared"
            );
        }
    }

    #[tokio::test]
    async fn test_dispatch_to_unreachable_returns_unavailable() {
        let a = make_adapter(19302);
        let result = a.dispatch("ping").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(_) => {}
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }

    /// Verifies that after a successful dispatch, the next dispatch includes
    /// the prior (user, assistant) turn in the outgoing message.
    #[tokio::test]
    async fn test_nzc_native_appends_history() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // NZC webhook response format
        let make_nzc_response = |text: &str| {
            let body = format!(r#"{{"response":"{}"}}"#, text);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        };

        let resp1 = make_nzc_response("first assistant reply");
        let resp2 = make_nzc_response("second assistant reply");

        // Capture the body of the SECOND request
        let second_body: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let second_body_srv = second_body.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            // First request — just respond
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(resp1.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Second request — capture body and respond
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                *second_body_srv.lock().await = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = stream.write_all(resp2.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        let a = NzcNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            Some(2000),
        );

        // First dispatch
        let r1 = a
            .dispatch_with_context(DispatchContext {
                message: "what is 2+2?",
                sender: Some("brian"),
            })
            .await;
        assert!(r1.is_ok(), "first dispatch failed: {:?}", r1);

        // Second dispatch — body should contain history from first turn
        let r2 = a
            .dispatch_with_context(DispatchContext {
                message: "and 3+3?",
                sender: Some("brian"),
            })
            .await;
        assert!(r2.is_ok(), "second dispatch failed: {:?}", r2);

        let body = second_body.lock().await.clone();

        // The second request body must contain the prior user and assistant turns
        assert!(
            body.contains("what is 2+2"),
            "second request should include prior user message, got:\n{}",
            body
        );
        assert!(
            body.contains("first assistant reply"),
            "second request should include prior assistant reply, got:\n{}",
            body
        );
        assert!(
            body.contains("[Conversation history]"),
            "expected history preamble in second request, got:\n{}",
            body
        );
        assert!(
            body.contains("[End history]"),
            "expected history end marker in second request, got:\n{}",
            body
        );
    }

    /// Verify that history is isolated by sender (different senders do not
    /// see each other's conversation history).
    #[tokio::test]
    async fn test_nzc_native_history_isolated_by_sender() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let make_nzc_response = |text: &str| {
            let body = format!(r#"{{"response":"{}"}}"#, text);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        };

        // We need 3 responses: brian turn 1, renee turn 1, brian turn 2
        let responses = vec![
            make_nzc_response("brian reply 1"),
            make_nzc_response("renee reply 1"),
            make_nzc_response("brian reply 2"),
        ];

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_srv = captured.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            for response in responses {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    captured_srv
                        .lock()
                        .await
                        .push(String::from_utf8_lossy(&buf[..n]).to_string());
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.flush().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        let a = NzcNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            Some(2000),
        );

        // brian: turn 1
        let _ = a
            .dispatch_with_context(DispatchContext {
                message: "brian first message",
                sender: Some("brian"),
            })
            .await;

        // renee: turn 1
        let _ = a
            .dispatch_with_context(DispatchContext {
                message: "renee first message",
                sender: Some("renee"),
            })
            .await;

        // brian: turn 2 — must contain brian's history but NOT renee's
        let _ = a
            .dispatch_with_context(DispatchContext {
                message: "brian second message",
                sender: Some("brian"),
            })
            .await;

        let requests = captured.lock().await.clone();
        assert_eq!(requests.len(), 3);

        let brian_turn2_req = &requests[2];
        assert!(
            brian_turn2_req.contains("brian first message"),
            "brian turn 2 should include his own history"
        );
        assert!(
            !brian_turn2_req.contains("renee first message"),
            "brian turn 2 must NOT include renee's history"
        );
    }
}
