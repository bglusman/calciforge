//! Shared channel telemetry helpers.
//!
//! Keep channel adapters on the same event schema so latency regressions can
//! be compared across Telegram, WhatsApp, Signal, SMS, and Matrix without scraping
//! channel-specific log text.

use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

pub fn delivery_lag_ms_from_unix_seconds(timestamp: u64) -> Option<u64> {
    if timestamp == 0 {
        return None;
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let sent_ms = u128::from(timestamp) * 1_000;
    if now_ms >= sent_ms {
        Some((now_ms - sent_ms).min(u128::from(u64::MAX)) as u64)
    } else {
        Some(0)
    }
}

pub fn authorized_message(
    channel: &'static str,
    identity: &str,
    sender: &str,
    text_len: usize,
    delivery_lag_ms: Option<u64>,
) {
    info!(
        channel,
        identity, sender, text_len, delivery_lag_ms, "authorized channel message"
    );
}

pub fn agent_dispatch_started(
    channel: &'static str,
    identity: &str,
    agent_id: &str,
    queue_wait_ms: u64,
) {
    info!(
        channel,
        identity, agent_id, queue_wait_ms, "channel agent dispatch started"
    );
}

pub fn command_reply_ready(
    channel: &'static str,
    identity: &str,
    command_kind: &'static str,
    queue_wait_ms: u64,
    handler_latency_ms: u64,
    response_len: usize,
) {
    info!(
        channel,
        identity,
        command_kind,
        queue_wait_ms,
        handler_latency_ms,
        response_len,
        "channel command reply ready"
    );
}

pub fn agent_dispatch_succeeded(
    channel: &'static str,
    identity: &str,
    agent_id: &str,
    dispatch_latency_ms: u64,
    response_len: usize,
) {
    info!(
        channel,
        identity, agent_id, dispatch_latency_ms, response_len, "channel agent dispatch succeeded"
    );
}

pub fn reply_sent(
    channel: &'static str,
    target: &str,
    reply_kind: &'static str,
    response_len: usize,
    send_latency_ms: u64,
) {
    info!(
        channel,
        target, reply_kind, response_len, send_latency_ms, "channel reply sent"
    );
}

pub fn reply_failed(
    channel: &'static str,
    target: &str,
    reply_kind: &'static str,
    send_latency_ms: u64,
    error: impl Display,
) {
    warn!(
        channel,
        target,
        reply_kind,
        send_latency_ms,
        error = %error,
        "channel reply failed"
    );
}
