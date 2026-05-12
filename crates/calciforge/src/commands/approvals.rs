use crate::adapters::openclaw::{PendingApprovalMeta, ZeroClawHttpAdapter};
use crate::messages::{ChoiceControl, ChoiceOption, OutboundMessage};

use super::CommandHandler;

impl CommandHandler {
    /// Handle a command that may require async work (approve/deny).
    ///
    /// Returns `Some((ack, Option<follow_up>))` if the text matches `!approve`
    /// or `!deny`, `None` if it is not a recognized async command.
    ///
    /// Callers should send `ack` immediately, then send `follow_up` (if present)
    /// once it arrives. It carries the continuation agent response after the
    /// approval or denial has been relayed to ZeroClaw and polled for a result.
    pub async fn handle_async(&self, text: &str) -> Option<(String, Option<String>)> {
        if Self::is_approve_command(text) {
            let (ack, follow_up) = self.handle_approve(text).await;
            Some((ack, follow_up))
        } else if Self::is_deny_command(text) {
            let (ack, follow_up) = self.handle_deny(text).await;
            Some((ack, follow_up))
        } else {
            None
        }
    }

    /// Register a pending approval for later `!approve` / `!deny` handling.
    ///
    /// Called by the channel dispatcher when it receives an `ApprovalPending`
    /// error from the router.
    pub async fn register_pending_approval(&self, meta: PendingApprovalMeta) {
        self.pending_approvals
            .lock()
            .await
            .insert(meta.request_id.clone(), meta);
    }

    /// Build the operator-facing approval request with reusable approve/deny choices.
    pub fn approval_request_message(
        command: &str,
        reason: &str,
        request_id: &str,
    ) -> OutboundMessage {
        let text = format!(
            "Approval required\nCommand: {command}\nReason: {reason}\nRequest ID: {request_id}"
        );
        OutboundMessage::text(text).with_control(ChoiceControl::new(
            "Choose an approval action",
            vec![
                ChoiceOption::approve(request_id),
                ChoiceOption::deny(request_id),
            ],
        ))
    }

    /// Handle an `!approve [request_id]` command.
    ///
    /// If no `request_id` is provided and exactly one approval is pending,
    /// auto-selects it. Signals ZeroClaw to allow the blocked tool call, then
    /// polls for the continuation result.
    ///
    /// Returns `(reply_message, Option<final_agent_response>)`.
    pub async fn handle_approve(&self, text: &str) -> (String, Option<String>) {
        let explicit_id = text.split_whitespace().nth(1);

        let meta = self.resolve_pending_approval(explicit_id).await;
        let meta = match meta {
            Ok(m) => m,
            Err(msg) => return (msg, None),
        };

        match ZeroClawHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
            true,
            None,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (format!("⚠️ Failed to send approval: {e}"), None);
            }
        }

        self.pending_approvals.lock().await.remove(&meta.request_id);

        let result = ZeroClawHttpAdapter::poll_result(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
        )
        .await;

        match result {
            Ok(response) => (
                format!("✅ Approved (request {})", meta.request_id),
                Some(response),
            ),
            Err(e) => (
                format!("✅ Approved — but failed to retrieve result: {e}"),
                None,
            ),
        }
    }

    /// Handle a `!deny [request_id] [reason]` command.
    ///
    /// If no `request_id` is provided and exactly one approval is pending,
    /// auto-selects it. Signals ZeroClaw to deny the blocked tool call, then
    /// polls for the continuation result.
    pub async fn handle_deny(&self, text: &str) -> (String, Option<String>) {
        let meta_and_reason = self.resolve_pending_denial(text).await;
        let (meta, reason) = match meta_and_reason {
            Ok(result) => result,
            Err(msg) => return (msg, None),
        };

        match ZeroClawHttpAdapter::send_approval_decision(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
            false,
            reason.as_deref(),
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                return (format!("⚠️ Failed to send denial: {e}"), None);
            }
        }

        self.pending_approvals.lock().await.remove(&meta.request_id);

        let result = ZeroClawHttpAdapter::poll_result(
            &self.http_client,
            &meta.zeroclaw_endpoint,
            &meta.zeroclaw_auth_token,
            &meta.request_id,
        )
        .await;

        match result {
            Ok(response) => (
                format!("🚫 Denied (request {})", meta.request_id),
                Some(response),
            ),
            Err(e) => (
                format!("🚫 Denied — but failed to retrieve result: {e}"),
                None,
            ),
        }
    }

    /// Resolve the pending approval to act on.
    ///
    /// If `explicit_id` is `Some`, looks up by that ID. If `None`, auto-selects
    /// the single pending approval or reports ambiguity.
    async fn resolve_pending_approval(
        &self,
        explicit_id: Option<&str>,
    ) -> Result<PendingApprovalMeta, String> {
        let store = self.pending_approvals.lock().await;
        if let Some(id) = explicit_id {
            match store.get(id) {
                Some(meta) => Ok(meta.clone()),
                None => Err(format!(
                    "⚠️ No pending approval with ID '{id}'.\n\nUse !approve or !deny without an ID to list pending approvals."
                )),
            }
        } else {
            match store.len() {
                0 => Err("⚠️ No pending approvals.".to_string()),
                1 => Ok(store.values().next().unwrap().clone()),
                n => {
                    let mut ids: Vec<&str> = store.keys().map(|s| s.as_str()).collect();
                    ids.sort_unstable();
                    Err(format!(
                        "⚠️ {n} pending approvals. Specify a request ID:\n{}",
                        ids.join("\n")
                    ))
                }
            }
        }
    }

    async fn resolve_pending_denial(
        &self,
        text: &str,
    ) -> Result<(PendingApprovalMeta, Option<String>), String> {
        let mut tokens = text.split_whitespace();
        let _command = tokens.next();
        let candidate = tokens.next();
        let remainder = tokens.collect::<Vec<_>>().join(" ");

        let store = self.pending_approvals.lock().await;
        if let Some(id) = candidate
            && let Some(meta) = store.get(id)
        {
            return Ok((meta.clone(), (!remainder.is_empty()).then_some(remainder)));
        }

        let reason = candidate.map(|first| {
            if remainder.is_empty() {
                first.to_string()
            } else {
                format!("{first} {remainder}")
            }
        });

        match store.len() {
            0 => Err("⚠️ No pending approvals.".to_string()),
            1 => Ok((store.values().next().unwrap().clone(), reason)),
            n => {
                let mut ids: Vec<&str> = store.keys().map(|s| s.as_str()).collect();
                ids.sort_unstable();
                Err(format!(
                    "⚠️ {n} pending approvals. Specify a request ID:\n{}",
                    ids.join("\n")
                ))
            }
        }
    }
}
