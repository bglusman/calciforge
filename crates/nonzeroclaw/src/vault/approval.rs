//! Approval relay — async, channel-agnostic human-in-the-loop gate.
//!
//! The relay accepts a secret access request, optionally routes it to a human
//! operator over a configured channel (Signal, Telegram, etc.), and waits for
//! a response.  The actual channel wiring (Signal/Telegram message delivery) is
//! plugged in via the `ChannelApprovalRelay` callback, keeping the relay itself
//! channel-agnostic.
//!
//! See `research/approval-relay-design.md` for the full design rationale.

use crate::vault::VaultError;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::oneshot;

// ── ApprovalDecision ─────────────────────────────────────────────────────────

/// The outcome of an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// The operator approved access.
    Approved,
    /// The operator explicitly denied access.
    Denied,
    /// No response was received within the TTL.
    TimedOut,
}

// ── ApprovalRelay trait ──────────────────────────────────────────────────────

/// Channel-agnostic approval relay.
///
/// Implementors handle the delivery of the request to the operator and wait for
/// a response.  The vault subsystem calls `request_approval` before releasing
/// a secret whose policy requires human sign-off.
#[async_trait]
pub trait ApprovalRelay: Send + Sync {
    /// Request operator approval for accessing `key`.
    ///
    /// `context` is a human-readable description of *why* the secret is needed
    /// (e.g. `"git push: 3 files changed in zeroclawed/src/"`).
    ///
    /// The call blocks until a decision arrives or the relay's internal timeout
    /// fires, at which point it returns `ApprovalDecision::TimedOut`.
    async fn request_approval(
        &self,
        key: &str,
        context: &str,
    ) -> Result<ApprovalDecision, VaultError>;
}

// ── NoopApprovalRelay ────────────────────────────────────────────────────────

/// An approval relay that always approves immediately.
///
/// Used for secrets with `policy = "auto"` — no human sign-off required.
/// Safe to use in automated pipelines and tests.
#[derive(Debug, Default, Clone)]
pub struct NoopApprovalRelay;

#[async_trait]
impl ApprovalRelay for NoopApprovalRelay {
    async fn request_approval(
        &self,
        _key: &str,
        _context: &str,
    ) -> Result<ApprovalDecision, VaultError> {
        Ok(ApprovalDecision::Approved)
    }
}

// ── ChannelApprovalRelay ─────────────────────────────────────────────────────

/// The callback type used by `ChannelApprovalRelay`.
///
/// The callback receives `(key, context, responder)`.  It must:
/// 1. Deliver the approval request to the operator over the chosen channel.
/// 2. Eventually call `responder.send(ApprovalDecision::...)` when the operator
///    responds (or when the channel-level timeout fires).
///
/// The callback takes ownership of the `oneshot::Sender` so the relay can
/// detect drops (which are treated as `TimedOut`).
pub type ApprovalCallback = Arc<
    dyn Fn(
            String,
            String,
            oneshot::Sender<ApprovalDecision>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// An approval relay that delegates delivery to a pluggable async callback.
///
/// The actual channel integration (Signal message, Telegram inline button, etc.)
/// is provided by the callback closure; this struct owns only the routing logic.
///
/// # Example (pseudocode — actual wiring belongs in the channel layer)
///
/// ```ignore
/// let relay = ChannelApprovalRelay::new(Arc::new(|key, ctx, tx| {
///     Box::pin(async move {
///         send_signal_message(&format!("Approve access to {key}? Context: {ctx}")).await;
///         // The channel message handler eventually calls tx.send(ApprovalDecision::Approved)
///         // The relay detects the drop/send and returns.
///         let _ = tx; // channel handler holds the sender
///     })
/// }));
/// ```
pub struct ChannelApprovalRelay {
    callback: ApprovalCallback,
    /// How long (in seconds) to wait before returning `TimedOut`.
    timeout_secs: u64,
}

impl ChannelApprovalRelay {
    /// Create a new relay with the given callback and timeout.
    pub fn new(callback: ApprovalCallback, timeout_secs: u64) -> Self {
        Self {
            callback,
            timeout_secs,
        }
    }

    /// Create a relay with a default 5-minute timeout.
    pub fn with_default_timeout(callback: ApprovalCallback) -> Self {
        Self::new(callback, 300)
    }
}

#[async_trait]
impl ApprovalRelay for ChannelApprovalRelay {
    async fn request_approval(
        &self,
        key: &str,
        context: &str,
    ) -> Result<ApprovalDecision, VaultError> {
        let (tx, rx) = oneshot::channel::<ApprovalDecision>();

        // Dispatch the delivery callback (fire-and-forget; the callback holds `tx`).
        let dispatch = (self.callback)(key.to_owned(), context.to_owned(), tx);
        tokio::spawn(dispatch);

        // Wait for either a response or a timeout.
        let timeout = tokio::time::Duration::from_secs(self.timeout_secs);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_sender_dropped)) => {
                // Callback dropped the sender without sending — treat as TimedOut.
                Ok(ApprovalDecision::TimedOut)
            }
            Err(_elapsed) => Ok(ApprovalDecision::TimedOut),
        }
    }
}

impl std::fmt::Debug for ChannelApprovalRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelApprovalRelay")
            .field("timeout_secs", &self.timeout_secs)
            .finish_non_exhaustive()
    }
}
