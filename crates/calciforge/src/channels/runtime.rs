//! Shared helpers for embedded channel runtimes.
//!
//! Keep channel transports at the edges, but centralize decisions that should
//! behave identically across embedded channels.

use adversary_detector::middleware::ChannelScanner;
use adversary_detector::verdict::{ScanContext, ScanVerdict};
use tracing::{debug, warn};
use zeroclaw_api::channel::ChannelMessage;

use crate::config::CalciforgeConfig;

pub fn reply_target(msg: &ChannelMessage) -> String {
    if msg.reply_target.is_empty() {
        msg.sender.clone()
    } else {
        msg.reply_target.clone()
    }
}

pub fn scan_enabled(config: &CalciforgeConfig, channel_kind: &str) -> bool {
    config
        .channels
        .iter()
        .find(|channel| channel.kind == channel_kind)
        .map(|channel| channel.scan_messages)
        .unwrap_or(false)
}

pub async fn inbound_scan_block_reply(
    channel_kind: &'static str,
    channel_label: &'static str,
    identity_id: &str,
    text: &str,
    scanner: &ChannelScanner,
    blocked_prefix: &'static str,
) -> Option<String> {
    match scanner.scan_text(text, ScanContext::UserMessage).await {
        ScanVerdict::Unsafe { reason } => {
            warn!(
                channel = channel_kind,
                identity = %identity_id,
                reason = %reason,
                "{channel_label}: inbound message BLOCKED by adversary scan"
            );
            Some(format!("{blocked_prefix}: {reason}"))
        }
        ScanVerdict::Review { reason } => {
            warn!(
                channel = channel_kind,
                identity = %identity_id,
                reason = %reason,
                "{channel_label}: inbound message flagged REVIEW - passing with caution"
            );
            None
        }
        ScanVerdict::Clean => {
            debug!(
                channel = channel_kind,
                identity = %identity_id,
                "{channel_label}: inbound scan clean"
            );
            None
        }
    }
}
