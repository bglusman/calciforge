//! Matrix channel adapter for ZeroClawed.
//!
//! Uses raw Matrix Client-Server API (HTTP long-polling / /sync).
//! No E2EE — matrix-sdk 0.16 has irreconcilable compile-time dependency
//! conflicts in this workspace (libsqlite3-sys version + recursion limit).
//! Plain-text messages only.
//!
//! ## Authentication model
//!
//! Matrix uses `allowed_users` (a list of Matrix user IDs) from the channel
//! config as the primary allowlist. Each allowed Matrix user is also matched
//! against the ZeroClawed identity table via the `matrix` channel alias.
//! If no alias is found, the Matrix user ID itself is used as the identity key.
//!
//! ## Invite handling
//!
//! The channel auto-accepts room invites from allowed users (DMs + group rooms).
//! Messages are processed in any joined room where the sender is in the allowlist.

use anyhow::{Context as _, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{debug, info, warn};

use crate::sync::Arc;
use crate::{
    auth::{find_agent, resolve_channel_sender},
    commands::CommandHandler,
    config::{expand_tilde, PolyConfig},
    context::ContextStore,
    router::Router,
};

// ---------------------------------------------------------------------------
// Matrix Client-Server API serde types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SyncResponse {
    next_batch: String,
    #[serde(default)]
    rooms: SyncRooms,
}

#[derive(Debug, Default, Deserialize)]
struct SyncRooms {
    #[serde(default)]
    join: HashMap<String, JoinedRoom>,
    #[serde(default)]
    invite: HashMap<String, InvitedRoom>,
}

#[derive(Debug, Default, Deserialize)]
struct JoinedRoom {
    #[serde(default)]
    timeline: Timeline,
}

#[derive(Debug, Default, Deserialize)]
struct Timeline {
    #[serde(default)]
    events: Vec<RoomEvent>,
}

#[derive(Debug, Deserialize)]
struct RoomEvent {
    #[serde(rename = "type")]
    event_type: String,
    event_id: Option<String>,
    sender: String,
    content: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
struct InvitedRoom {
    #[serde(rename = "invite_state", default)]
    invite_state: InviteState,
}

#[derive(Debug, Default, Deserialize)]
struct InviteState {
    #[serde(default)]
    events: Vec<StrippedEvent>,
}

#[derive(Debug, Deserialize)]
struct StrippedEvent {
    #[serde(rename = "type")]
    event_type: String,
    sender: String,
    #[serde(default)]
    state_key: String,
    content: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let safe = matches!(
            byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
        );
        if safe {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn is_sender_allowed(allowed_users: &[String], sender: &str) -> bool {
    if allowed_users.iter().any(|u| u == "*") {
        return true;
    }
    allowed_users.iter().any(|u| u.eq_ignore_ascii_case(sender))
}

fn cache_event_id(
    event_id: &str,
    recent_order: &mut VecDeque<String>,
    recent_lookup: &mut HashSet<String>,
) -> bool {
    const MAX_RECENT_EVENT_IDS: usize = 2048;

    if recent_lookup.contains(event_id) {
        return true; // duplicate
    }

    recent_lookup.insert(event_id.to_string());
    recent_order.push_back(event_id.to_string());

    if recent_order.len() > MAX_RECENT_EVENT_IDS {
        if let Some(evicted) = recent_order.pop_front() {
            recent_lookup.remove(&evicted);
        }
    }

    false
}

async fn resolve_room_id(
    homeserver: &str,
    room_id_config: &str,
    http: &reqwest::Client,
    auth_header: &str,
) -> Result<String> {
    let configured = room_id_config.trim();

    if configured.starts_with('!') {
        return Ok(configured.to_string());
    }

    if configured.starts_with('#') {
        let encoded = encode_path_segment(configured);
        let url = format!(
            "{}/_matrix/client/v3/directory/room/{}",
            homeserver, encoded
        );
        let resp = http
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix room alias resolution failed for '{configured}': {err}");
        }
        #[derive(Deserialize)]
        struct RoomAliasResp {
            room_id: String,
        }
        let resolved: RoomAliasResp = resp.json().await?;
        return Ok(resolved.room_id);
    }

    anyhow::bail!(
        "Matrix room_id must start with '!' (room ID) or '#' (room alias), got: {configured}"
    )
}

async fn get_whoami(
    homeserver: &str,
    http: &reqwest::Client,
    auth_header: &str,
) -> Result<(String, Option<String>)> {
    let url = format!("{}/_matrix/client/v3/account/whoami", homeserver);
    let resp = http
        .get(&url)
        .header("Authorization", auth_header)
        .send()
        .await?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix whoami failed: {err}");
    }
    #[derive(Deserialize)]
    struct WhoAmI {
        user_id: String,
        device_id: Option<String>,
    }
    let w: WhoAmI = resp.json().await?;
    Ok((w.user_id, w.device_id))
}

async fn ensure_room_accessible(
    homeserver: &str,
    room_id: &str,
    http: &reqwest::Client,
    auth_header: &str,
) -> Result<()> {
    let encoded = encode_path_segment(room_id);
    let url = format!(
        "{}/_matrix/client/v3/rooms/{}/joined_members",
        homeserver, encoded
    );
    let resp = http
        .get(&url)
        .header("Authorization", auth_header)
        .send()
        .await?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix room access check failed for '{room_id}': {err}");
    }
    Ok(())
}

async fn check_room_encryption(
    homeserver: &str,
    room_id: &str,
    http: &reqwest::Client,
    auth_header: &str,
) -> bool {
    let encoded = encode_path_segment(room_id);
    let url = format!(
        "{}/_matrix/client/v3/rooms/{}/state/m.room.encryption",
        homeserver, encoded
    );
    let Ok(resp) = http
        .get(&url)
        .header("Authorization", auth_header)
        .send()
        .await
    else {
        return false;
    };
    resp.status().is_success()
}

// ---------------------------------------------------------------------------
// Raw HTTP Matrix operations
// ---------------------------------------------------------------------------

async fn do_sync(
    homeserver: &str,
    http: &reqwest::Client,
    auth_header: &str,
    since: Option<&str>,
    timeout_ms: u64,
) -> Result<SyncResponse> {
    let mut url = format!(
        "{}/_matrix/client/v3/sync?timeout={}",
        homeserver, timeout_ms
    );
    if let Some(s) = since {
        url.push_str("&since=");
        url.push_str(s);
    }
    let resp = http
        .get(&url)
        .header("Authorization", auth_header)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix /sync failed ({status}): {err}");
    }
    Ok(resp.json::<SyncResponse>().await?)
}

async fn send_matrix_message(
    homeserver: &str,
    http: &reqwest::Client,
    auth_header: &str,
    room_id: &str,
    body: &str,
) -> Result<()> {
    let encoded_room = encode_path_segment(room_id);
    let txn_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    let url = format!(
        "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
        homeserver, encoded_room, txn_id
    );
    let payload = serde_json::json!({
        "msgtype": "m.text",
        "body": body,
    });
    let resp = http
        .put(&url)
        .header("Authorization", auth_header)
        .json(&payload)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix send failed ({status}): {err}");
    }
    Ok(())
}

async fn join_matrix_room(
    homeserver: &str,
    http: &reqwest::Client,
    auth_header: &str,
    room_id: &str,
) -> Result<()> {
    let encoded_room = encode_path_segment(room_id);
    let url = format!("{}/_matrix/client/v3/join/{}", homeserver, encoded_room);
    let resp = http
        .post(&url)
        .header("Authorization", auth_header)
        .json(&serde_json::json!({}))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix join failed ({status}): {err}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// pub run()
// ---------------------------------------------------------------------------

pub async fn run(
    config: Arc<PolyConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
) -> Result<()> {
    let channel = config
        .channels
        .iter()
        .find(|c| c.kind == "matrix" && c.enabled);

    let channel = match channel {
        Some(c) => c.clone(),
        None => {
            info!("No enabled Matrix channel found in config — Matrix adapter not started.");
            return Ok(());
        }
    };

    let homeserver = channel
        .homeserver
        .as_deref()
        .context("Matrix channel missing `homeserver` in config")?
        .trim_end_matches('/')
        .to_string();

    let token_file = channel
        .access_token_file
        .as_deref()
        .context("Matrix channel missing `access_token_file` in config")?;

    let room_id_config: Option<String> = channel.room_id.as_deref().map(|s| s.to_string());

    let allowed_users: Vec<String> = channel
        .allowed_users
        .iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();

    if allowed_users.is_empty() {
        anyhow::bail!("Matrix channel requires at least one allowed_user for security");
    }

    let access_token = std::fs::read_to_string(expand_tilde(token_file))
        .with_context(|| format!("Matrix: failed to read access_token_file '{token_file}'"))?
        .trim()
        .to_string();

    let auth_header = format!("Bearer {}", access_token);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    // Resolve optional configured room
    let target_room: Option<String> = if let Some(ref room_cfg) = room_id_config {
        let room_id_str = resolve_room_id(&homeserver, room_cfg, &http, &auth_header)
            .await
            .with_context(|| format!("Matrix: failed to resolve room '{room_cfg}'"))?;
        info!(room_id = %room_id_str, "Matrix room resolved");

        ensure_room_accessible(&homeserver, &room_id_str, &http, &auth_header)
            .await
            .with_context(|| format!("Matrix: room '{room_id_str}' not accessible"))?;

        let is_encrypted =
            check_room_encryption(&homeserver, &room_id_str, &http, &auth_header).await;
        if is_encrypted {
            warn!(
                room_id = %room_id_str,
                "Matrix room has E2EE enabled, but this build uses plain-text messaging only. \
                 Messages will fail. Disable E2EE on the room or use a non-encrypted room."
            );
        }
        Some(room_id_str)
    } else {
        info!("Matrix: no room_id configured — accepting messages from any joined room");
        None
    };

    let (my_user_id, _device_id) = get_whoami(&homeserver, &http, &auth_header).await?;
    info!(user_id = %my_user_id, "Matrix bot identity confirmed");

    // Initial sync: grab next_batch but discard all events (skip backlog)
    info!("Matrix: performing initial sync to skip backlog...");
    let initial = do_sync(&homeserver, &http, &auth_header, None, 0)
        .await
        .context("Matrix: initial sync failed")?;
    let mut since = initial.next_batch;
    info!("Matrix: initial sync complete — listening for new events");

    info!(
        target_room = ?target_room.as_deref(),
        user_id = %my_user_id,
        allowed_users = ?allowed_users,
        "Matrix channel listening"
    );

    let mut dedup_order: VecDeque<String> = VecDeque::new();
    let mut dedup_lookup: HashSet<String> = HashSet::new();
    let mut retry_delay: u64 = 5;

    loop {
        let sync_result = do_sync(&homeserver, &http, &auth_header, Some(&since), 30_000).await;

        let sync = match sync_result {
            Ok(s) => {
                retry_delay = 5;
                s
            }
            Err(e) => {
                warn!(error = %e, retry_delay, "Matrix: sync error, retrying");
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay)).await;
                retry_delay = (retry_delay * 2).min(60);
                continue;
            }
        };

        since = sync.next_batch;

        // --- Process invites: auto-accept from allowed users ---
        for (room_id, invited) in &sync.rooms.invite {
            let inviter = invited
                .invite_state
                .events
                .iter()
                .find(|e| {
                    e.event_type == "m.room.member"
                        && e.state_key == my_user_id
                        && e.content.get("membership").and_then(|m| m.as_str()) == Some("invite")
                })
                .map(|e| e.sender.as_str())
                .unwrap_or("");

            if !is_sender_allowed(&allowed_users, inviter) {
                debug!(room_id = %room_id, inviter = %inviter, "Matrix: ignoring invite from non-allowed user");
                continue;
            }

            info!(room_id = %room_id, inviter = %inviter, "Matrix: auto-accepting invite from allowed user");
            if let Err(e) = join_matrix_room(&homeserver, &http, &auth_header, room_id).await {
                warn!(room_id = %room_id, error = %e, "Matrix: failed to join room after invite");
            }
        }

        // --- Process messages from joined rooms ---
        for (room_id, joined) in &sync.rooms.join {
            // If a target room is configured, skip other rooms
            if let Some(ref tr) = target_room {
                if room_id != tr {
                    continue;
                }
            }

            for event in &joined.timeline.events {
                if event.event_type != "m.room.message" {
                    continue;
                }

                // Skip our own messages
                if event.sender == my_user_id {
                    continue;
                }

                // Allowlist check
                if !is_sender_allowed(&allowed_users, &event.sender) {
                    debug!(sender = %event.sender, "Matrix: dropping message from non-allowed user");
                    continue;
                }

                // Extract plain text body (m.text and m.notice only)
                let msgtype = event
                    .content
                    .get("msgtype")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if msgtype != "m.text" && msgtype != "m.notice" {
                    continue;
                }
                let body = match event.content.get("body").and_then(|v| v.as_str()) {
                    Some(b) if !b.trim().is_empty() => b.to_string(),
                    _ => continue,
                };

                // Deduplication
                let event_id = event
                    .event_id
                    .clone()
                    .unwrap_or_else(|| format!("no-id-{}-{}", room_id, event.sender));
                if cache_event_id(&event_id, &mut dedup_order, &mut dedup_lookup) {
                    debug!(event_id = %event_id, "Matrix: duplicate event, skipping");
                    continue;
                }

                info!(
                    sender = %event.sender,
                    room_id = %room_id,
                    event_id = %event_id,
                    body_len = body.len(),
                    "Matrix: received message"
                );

                // Resolve identity
                let identity = resolve_channel_sender("matrix", &event.sender, &config);
                let identity_id = identity
                    .as_ref()
                    .map(|i| i.id.clone())
                    .unwrap_or_else(|| event.sender.clone());
                let chat_key = format!("matrix-{}", identity_id);

                // Dispatch in a separate task so we don't block the sync loop
                let homeserver = homeserver.clone();
                let auth_header = auth_header.clone();
                let http = http.clone();
                let room_id = room_id.clone();
                let sender = event.sender.clone();
                let config = config.clone();
                let router = router.clone();
                let command_handler = command_handler.clone();
                let context_store = context_store.clone();

                tokio::spawn(async move {
                    handle_message(
                        &homeserver,
                        &http,
                        &auth_header,
                        &room_id,
                        &sender,
                        &identity_id,
                        &chat_key,
                        &body,
                        &config,
                        &router,
                        &command_handler,
                        &context_store,
                    )
                    .await;
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Message handling (runs in spawned task)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    homeserver: &str,
    http: &reqwest::Client,
    auth_header: &str,
    room_id: &str,
    sender: &str,
    identity_id: &str,
    chat_key: &str,
    body: &str,
    config: &Arc<PolyConfig>,
    router: &Arc<Router>,
    cmd_handler: &Arc<CommandHandler>,
    ctx_store: &ContextStore,
) {
    let send = |text: String| {
        let homeserver = homeserver.to_string();
        let http = http.clone();
        let auth_header = auth_header.to_string();
        let room_id = room_id.to_string();
        async move {
            if let Err(e) =
                send_matrix_message(&homeserver, &http, &auth_header, &room_id, &text).await
            {
                warn!(error = %e, "Matrix: failed to send message");
            }
        }
    };

    // --- Command fast-path ---
    if let Some(reply) = cmd_handler.handle(body) {
        debug!(sender = %sender, cmd = %body.trim(), "Matrix: handled local command");
        send(reply).await;
        return;
    }

    // Unknown !command
    if CommandHandler::is_command(body)
        && !CommandHandler::is_status_command(body)
        && !CommandHandler::is_switch_command(body)
        && !CommandHandler::is_default_command(body)
        && !CommandHandler::is_sessions_command(body)
        && !CommandHandler::is_model_command(body)
    {
        send(cmd_handler.unknown_command(body)).await;
        return;
    }

    if CommandHandler::is_status_command(body) {
        let reply = cmd_handler.cmd_status_for_identity(identity_id).await;
        send(reply).await;
        return;
    }

    if CommandHandler::is_switch_command(body) {
        send(cmd_handler.handle_switch(body, identity_id)).await;
        return;
    }

    if CommandHandler::is_model_command(body) {
        send(cmd_handler.handle_model(body, identity_id)).await;
        return;
    }

    if CommandHandler::is_sessions_command(body) {
        let reply = cmd_handler.handle_sessions(body, identity_id).await;
        send(reply).await;
        return;
    }

    if CommandHandler::is_default_command(body) {
        send(cmd_handler.handle_default(identity_id)).await;
        return;
    }

    if body.trim().eq_ignore_ascii_case("!context clear") {
        ctx_store.clear(chat_key);
        send("Conversation context cleared.".to_string()).await;
        return;
    }

    if CommandHandler::is_approve_command(body) || CommandHandler::is_deny_command(body) {
        debug!(sender = %sender, cmd = %body.trim(), "Matrix: handling async approval command");
        if let Some((ack, follow_up)) = cmd_handler.handle_async(body).await {
            send(ack).await;
            if let Some(resp) = follow_up {
                send(resp).await;
            }
        }
        return;
    }

    // --- Agent dispatch ---
    let agent_id = match cmd_handler.active_agent_for(identity_id) {
        Some(id) => id,
        None => {
            warn!(sender = %sender, identity = %identity_id, "Matrix: no routing rule — dropping");
            return;
        }
    };

    let agent = match find_agent(&agent_id, config) {
        Some(a) => a.clone(),
        None => {
            warn!(agent_id = %agent_id, "Matrix: agent not found in config");
            send("Agent not configured.".to_string()).await;
            return;
        }
    };

    let sender_label = config
        .identities
        .iter()
        .find(|i| i.id == *identity_id)
        .and_then(|i| i.display_name.as_deref())
        .unwrap_or(identity_id)
        .to_string();

    let augmented = ctx_store.augment_message(chat_key, &agent_id, body);
    let dispatch_start = std::time::Instant::now();

    match router
        .dispatch_with_sender(&augmented, &agent, config, Some(identity_id))
        .await
    {
        Ok(response) => {
            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
            cmd_handler.record_dispatch(latency_ms);
            debug!(
                identity = %identity_id,
                agent_id = %agent_id,
                response_len = response.len(),
                "Matrix: got agent response"
            );
            ctx_store.push(chat_key, &sender_label, body, &agent_id, &response);
            send(response).await;
        }
        Err(e) => {
            // Clash approval flow
            if let Some(crate::adapters::AdapterError::ApprovalPending(req)) =
                e.downcast_ref::<crate::adapters::AdapterError>()
            {
                let req = req.clone();
                debug!(
                    request_id = %req.request_id,
                    command = %req.command,
                    "Matrix: clash approval request — forwarding to user"
                );
                cmd_handler
                    .register_pending_approval(
                        crate::adapters::openclaw::PendingApprovalMeta {
                            request_id: req.request_id.clone(),
                            nzc_endpoint: agent.endpoint.clone(),
                            nzc_auth_token: agent.auth_token.clone().unwrap_or_default(),
                            _summary: format!(
                                "Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                                req.command, req.reason, req.request_id
                            ),
                        },
                    )
                    .await;
                let notification = format!(
                    "Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                    req.command, req.reason, req.request_id
                );
                send(notification).await;
                return;
            }
            warn!(identity = %identity_id, error = %e, "Matrix: agent dispatch failed");
            send(format!("Agent error: {}", e)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_path_segment_safe_chars() {
        let safe = "ABC-xyz_123.~";
        assert_eq!(encode_path_segment(safe), safe);
    }

    #[test]
    fn test_encode_path_segment_special_chars() {
        assert_eq!(encode_path_segment("hello world"), "hello%20world");
        assert_eq!(encode_path_segment("foo@bar"), "foo%40bar");
        assert_eq!(encode_path_segment("test#hash"), "test%23hash");
    }

    #[test]
    fn test_encode_path_segment_unicode() {
        assert_eq!(encode_path_segment("café"), "caf%C3%A9");
    }

    #[test]
    fn test_is_sender_allowed_exact_match() {
        let allowed = vec!["@alice:matrix.org".to_string()];
        assert!(is_sender_allowed(&allowed, "@alice:matrix.org"));
    }

    #[test]
    fn test_is_sender_allowed_case_insensitive() {
        let allowed = vec!["@ALICE:MATRIX.ORG".to_string()];
        assert!(is_sender_allowed(&allowed, "@alice:matrix.org"));
        assert!(is_sender_allowed(&allowed, "@Alice:Matrix.org"));
    }

    #[test]
    fn test_is_sender_allowed_wildcard() {
        let allowed = vec!["*".to_string()];
        assert!(is_sender_allowed(&allowed, "@anyone:anywhere"));
        assert!(is_sender_allowed(&allowed, ""));
    }

    #[test]
    fn test_is_sender_allowed_not_in_list() {
        let allowed = vec!["@alice:matrix.org".to_string()];
        assert!(!is_sender_allowed(&allowed, "@bob:matrix.org"));
        assert!(!is_sender_allowed(&allowed, ""));
    }

    #[test]
    fn test_is_sender_allowed_empty_list() {
        let allowed: Vec<String> = vec![];
        assert!(!is_sender_allowed(&allowed, "@alice:matrix.org"));
    }

    #[test]
    fn test_cache_event_id_new_event() {
        let mut order = VecDeque::new();
        let mut lookup = HashSet::new();
        let is_dup = cache_event_id("event123", &mut order, &mut lookup);
        assert!(!is_dup);
        assert!(lookup.contains("event123"));
        assert_eq!(order.len(), 1);
    }

    #[test]
    fn test_cache_event_id_duplicate() {
        let mut order = VecDeque::new();
        let mut lookup = HashSet::new();
        cache_event_id("event123", &mut order, &mut lookup);
        let is_dup = cache_event_id("event123", &mut order, &mut lookup);
        assert!(is_dup);
        assert_eq!(order.len(), 1);
    }

    #[test]
    fn test_cache_event_id_eviction() {
        let mut order = VecDeque::new();
        let mut lookup = HashSet::new();
        for i in 0..2050 {
            cache_event_id(&format!("event{}", i), &mut order, &mut lookup);
        }
        assert_eq!(order.len(), 2048);
        assert!(!lookup.contains("event0"));
        assert!(!lookup.contains("event1"));
        assert!(lookup.contains("event2049"));
    }
}
