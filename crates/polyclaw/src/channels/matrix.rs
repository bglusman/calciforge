//! Matrix channel adapter for PolyClaw v3.
//!
//! ## Architecture
//!
//! 1. Connect to Matrix homeserver using the access token from `access_token_file`
//! 2. Listen for invites from `allowed_users` and auto-accept DM invites
//! 3. Listen for `m.room.message` events from `allowed_users` in any room (DMs + configured room)
//! 4. Route each message to the active agent via `Router::dispatch()`
//! 5. Send the agent's response back to the room as a `m.room.message`
//!
//! ## Authentication model
//!
//! Matrix uses `allowed_users` (a list of Matrix user IDs) from the channel config
//! as the primary allowlist, mirroring the NonZeroClaw approach.  Each allowed
//! Matrix user is also matched against the PolyClaw identity table via the
//! `matrix` channel alias (e.g. `{ channel = "matrix", id = "@brian:matrix.org" }`).
//! If no identity alias is found, the Matrix user ID itself is used as the
//! identity key (for routing / context isolation).
//!
//! ## DM Support
//!
//! The channel auto-accepts invites from allowed users, enabling 1:1 DMs.
//! Messages are processed in any room where the sender is in the allowlist.
//! The optional `room_id` config can still be used for explicit room-based routing.

#[cfg(not(feature = "channel-matrix"))]
pub async fn run(
    config: std::sync::Arc<crate::config::PolyConfig>,
    _router: std::sync::Arc<crate::router::Router>,
    _command_handler: std::sync::Arc<crate::commands::CommandHandler>,
    _context_store: crate::context::ContextStore,
) -> anyhow::Result<()> {
    let has_matrix = config
        .channels
        .iter()
        .any(|c| c.kind == "matrix" && c.enabled);

    if has_matrix {
        tracing::warn!(
            "Matrix channel is enabled in config but PolyClaw was built without \
             the `channel-matrix` feature. Rebuild with `--features channel-matrix` \
             to activate the Matrix adapter."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Real implementation — only compiled when feature = "channel-matrix"
// ---------------------------------------------------------------------------

#[cfg(feature = "channel-matrix")]
mod inner {
    use anyhow::{Context as _, Result};
    use matrix_sdk::{
        authentication::matrix::MatrixSession,
        config::SyncSettings,
        ruma::{
            events::room::member::{MembershipState, StrippedRoomMemberEvent},
            events::room::message::{
                MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
            },
            OwnedRoomId, OwnedUserId,
        },
        Client as MatrixSdkClient, LoopCtrl, Room, RoomState, SessionMeta, SessionTokens,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tracing::{debug, info, warn};

    use crate::{
        auth::{find_agent, resolve_channel_sender},
        commands::CommandHandler,
        config::{expand_tilde, PolyConfig},
        context::ContextStore,
        router::Router,
    };

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

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

    /// Check if a room is a DM (direct message) with only 2 members
    async fn is_dm_room(room: &Room) -> bool {
        match room.members(matrix_sdk::RoomMemberships::ACTIVE).await {
            Ok(members) => members.len() == 2,
            Err(_) => false,
        }
    }

    fn cache_event_id(
        event_id: &str,
        recent_order: &mut std::collections::VecDeque<String>,
        recent_lookup: &mut std::collections::HashSet<String>,
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
            #[derive(serde::Deserialize)]
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
        #[derive(serde::Deserialize)]
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

    async fn build_matrix_client(
        homeserver: &str,
        access_token: &str,
        user_id: &str,
        device_id: &str,
        store_dir: Option<std::path::PathBuf>,
    ) -> Result<MatrixSdkClient> {
        info!("Matrix: building client with store...");
        let mut builder = MatrixSdkClient::builder().homeserver_url(homeserver);

        if let Some(dir) = store_dir {
            info!(store_dir = %dir.display(), "Matrix: creating sqlite store dir");
            tokio::fs::create_dir_all(&dir).await.with_context(|| {
                format!(
                    "Matrix: failed to create sqlite store dir at {}",
                    dir.display()
                )
            })?;
            builder = builder.sqlite_store(&dir, None);
        }

        info!("Matrix: building client...");
        let client = builder.build().await?;
        info!("Matrix: client built");

        let uid: OwnedUserId = user_id
            .parse()
            .with_context(|| format!("Matrix: invalid user_id '{user_id}'"))?;

        let session = MatrixSession {
            meta: SessionMeta {
                user_id: uid,
                device_id: device_id.into(),
            },
            tokens: SessionTokens {
                access_token: access_token.to_string(),
                refresh_token: None,
            },
        };

        info!("Matrix: restoring session...");
        client.restore_session(session).await?;
        info!("Matrix: session restored");
        Ok(client)
    }

    // -----------------------------------------------------------------------
    // pub run()
    // -----------------------------------------------------------------------

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

        // --- Read required config fields ---

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

        // room_id is now optional — if provided, we verify access; if not, we rely on DMs only
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

        // Read access token from file
        let access_token = std::fs::read_to_string(expand_tilde(token_file))
            .with_context(|| format!("Matrix: failed to read access_token_file '{token_file}'"))?
            .trim()
            .to_string();

        let auth_header = format!("Bearer {}", access_token);
        let http = reqwest::Client::new();

        // --- Resolve room ID if provided (alias → canonical) ---
        let target_room: Option<OwnedRoomId> = if let Some(ref room_cfg) = room_id_config {
            let room_id_str = resolve_room_id(&homeserver, room_cfg, &http, &auth_header)
                .await
                .with_context(|| format!("Matrix: failed to resolve room '{room_cfg}'"))?;

            info!(room_id = %room_id_str, "Matrix room resolved");

            // --- Verify room accessibility ---
            ensure_room_accessible(&homeserver, &room_id_str, &http, &auth_header)
                .await
                .with_context(|| format!("Matrix: room '{room_id_str}' not accessible"))?;

            let is_encrypted =
                check_room_encryption(&homeserver, &room_id_str, &http, &auth_header).await;
            if is_encrypted {
                info!(room_id = %room_id_str, "Matrix room is encrypted — E2EE enabled via matrix-sdk");
            }

            Some(
                room_id_str
                    .parse()
                    .with_context(|| format!("Matrix: invalid room ID '{room_id_str}'"))?,
            )
        } else {
            info!("Matrix: no room_id configured — operating in DM-only mode");
            None
        };

        // --- Whoami ---
        let (my_user_id_str, my_device_id_opt) =
            get_whoami(&homeserver, &http, &auth_header).await?;
        let my_device_id = my_device_id_opt.context(
            "Matrix whoami did not return a device_id — needed for E2EE session restore",
        )?;

        info!(user_id = %my_user_id_str, device_id = %my_device_id, "Matrix bot identity confirmed");

        // --- Build matrix-sdk client ---
        // Store E2EE keys in ~/.polyclaw/state/matrix/
        info!("Matrix: preparing store directory...");
        let store_dir = {
            let home = home::home_dir();
            home.map(|h| h.join(".polyclaw").join("state").join("matrix"))
        };

        info!(store_dir = ?store_dir, "Matrix: building matrix-sdk client...");
        let client = build_matrix_client(
            &homeserver,
            &access_token,
            &my_user_id_str,
            &my_device_id,
            store_dir,
        )
        .await
        .context("Matrix: failed to build matrix-sdk client")?;
        info!("Matrix: client ready");

        // Log E2EE device status
        info!("Matrix: checking device status...");
        match client.encryption().get_own_device().await {
            Ok(Some(device)) => {
                if device.is_verified() {
                    info!(device_id = %my_device_id, "Matrix device is verified for E2EE");
                } else {
                    warn!(
                        device_id = %my_device_id,
                        "Matrix device is NOT verified. Messages may be flagged as unverified"
                    );
                }
            }
            Ok(None) => warn!("Matrix device metadata unavailable; verification status unknown"),
            Err(e) => warn!(error = %e, "Matrix device verification check failed"),
        }

        // Initial sync to mark existing messages as seen (don't process backlog)
        info!("Matrix: performing initial sync...");
        let _ = client.sync_once(SyncSettings::new()).await;
        info!("Matrix: initial sync complete");

        info!(
            target_room = ?target_room.as_ref().map(|r| r.as_str()),
            user_id = %my_user_id_str,
            allowed_users = ?allowed_users,
            "Matrix channel listening (DMs + target room)"
        );

        // --- Register event handlers ---
        let my_user_id: OwnedUserId = my_user_id_str
            .parse()
            .with_context(|| format!("Matrix: invalid user_id '{my_user_id_str}'"))?;

        let dedup_cache: Arc<
            Mutex<(
                std::collections::VecDeque<String>,
                std::collections::HashSet<String>,
            )>,
        > = Arc::new(Mutex::new((
            std::collections::VecDeque::new(),
            std::collections::HashSet::new(),
        )));

        let config_h = config.clone();
        let router_h = router.clone();
        let cmd_handler_h = command_handler.clone();
        let ctx_store_h = context_store.clone();
        let allowed_users_h = allowed_users.clone();
        let dedup_h = Arc::clone(&dedup_cache);
        let target_room_h = target_room.clone();
        let my_user_id_h = my_user_id.clone();
        let client_h = client.clone();

        // --- Invite handler: auto-accept DMs from allowed users ---
        let allowed_users_invite = allowed_users.clone();
        let my_user_id_invite = my_user_id.clone();
        client.add_event_handler(
            move |event: StrippedRoomMemberEvent, room: Room| {
                let allowed_users = allowed_users_invite.clone();
                let my_user_id = my_user_id_invite.clone();
                async move {
                    // Only process invites to us
                    if event.state_key != my_user_id {
                        return;
                    }
                    if event.content.membership != MembershipState::Invite {
                        return;
                    }
                    let sender = event.sender.to_string();
                    if !is_sender_allowed(&allowed_users, &sender) {
                        debug!(sender = %sender, "Matrix: ignoring invite from non-allowed user");
                        return;
                    }
                    info!(sender = %sender, room_id = %room.room_id(), "Matrix: auto-accepting invite from allowed user");
                    if let Err(e) = room.join().await {
                        warn!(error = %e, "Matrix: failed to join room after invite");
                    }
                }
            }
        );

        // --- Message handler: process messages from allowed users in any room ---
        client.add_event_handler(
            move |event: OriginalSyncRoomMessageEvent, room: Room| {
                let config = config_h.clone();
                let router = router_h.clone();
                let cmd_handler = cmd_handler_h.clone();
                let ctx_store = ctx_store_h.clone();
                let allowed_users = allowed_users_h.clone();
                let dedup = Arc::clone(&dedup_h);
                let target_room = target_room_h.clone();
                let my_user_id = my_user_id_h.clone();

                async move {
                    // Log all incoming message events for debugging
                    info!(sender = %event.sender, room_id = %room.room_id(), msg_type = ?event.content.msgtype, "Matrix: received message event");

                    // Ignore our own messages
                    if event.sender == my_user_id {
                        debug!("Matrix: ignoring own message");
                        return;
                    }

                    let sender = event.sender.to_string();

                    // Allowlist check — if not allowed, drop immediately
                    if !is_sender_allowed(&allowed_users, &sender) {
                        info!(sender = %sender, allowed = ?allowed_users, "Matrix: dropping message from non-allowed user");
                        return;
                    }

                    // Determine if we should process this message:
                    // 1. It's in the configured target room (if any), OR
                    // 2. It's a DM (2-member room)
                    let in_target_room = target_room.as_ref()
                        .map(|tr| room.room_id().as_str() == tr.as_str())
                        .unwrap_or(false);
                    let is_dm = is_dm_room(&room).await;

                    info!(room_id = %room.room_id(), in_target_room, is_dm, "Matrix: processing message");

                    if !in_target_room && !is_dm {
                        info!(room_id = %room.room_id(), "Matrix: ignoring message in non-target, non-DM room");
                        return;
                    }

                    // Extract body text (m.text and m.notice only)
                    let body = match &event.content.msgtype {
                        MessageType::Text(c) => c.body.clone(),
                        MessageType::Notice(c) => c.body.clone(),
                        _ => return,
                    };

                    if body.trim().is_empty() {
                        return;
                    }

                    // Deduplication
                    let event_id = event.event_id.to_string();
                    {
                        let mut guard = dedup.lock().await;
                        let (ref mut order, ref mut lookup) = *guard;
                        if cache_event_id(&event_id, order, lookup) {
                            debug!(event_id = %event_id, "Matrix: duplicate event, skipping");
                            return;
                        }
                    }

                    info!(
                        sender = %sender,
                        event_id = %event_id,
                        body_len = %body.len(),
                        "Matrix: received message"
                    );

                    // Resolve identity (for routing context isolation)
                    // Try to match Matrix user ID against identity aliases first.
                    let identity = resolve_channel_sender("matrix", &sender, &config);
                    let identity_id = identity
                        .as_ref()
                        .map(|i| i.id.clone())
                        .unwrap_or_else(|| sender.clone());

                    let chat_key = format!("matrix-{}", identity_id);

                    // --- Command fast-path (synchronous, no agent I/O) ---
                    if let Some(reply) = cmd_handler.handle(&body) {
                        debug!(sender = %sender, cmd = %body.trim(), "Matrix: handled local command");
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send command reply");
                            }
                        });
                        return;
                    }

                    // Unknown !command handling
                    if CommandHandler::is_command(&body)
                        && !CommandHandler::is_status_command(&body)
                        && !CommandHandler::is_switch_command(&body)
                        && !CommandHandler::is_default_command(&body)
                        && !CommandHandler::is_sessions_command(&body)
                    {
                        let reply = cmd_handler.unknown_command(&body);
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send unknown-command reply");
                            }
                        });
                        return;
                    }

                    // !status — post-auth identity command
                    if CommandHandler::is_status_command(&body) {
                        let reply = cmd_handler.cmd_status_for_identity(&identity_id).await;
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send status reply");
                            }
                        });
                        return;
                    }

                    // !switch — post-auth agent switch
                    if CommandHandler::is_switch_command(&body) {
                        let reply = cmd_handler.handle_switch(&body, &identity_id);
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send switch reply");
                            }
                        });
                        return;
                    }

                    // !sessions — list ACP sessions for an agent
                    if CommandHandler::is_sessions_command(&body) {
                        let reply = cmd_handler.handle_sessions(&body, &identity_id).await;
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send sessions reply");
                            }
                        });
                        return;
                    }

                    // !default — reset to default agent
                    if CommandHandler::is_default_command(&body) {
                        let reply = cmd_handler.handle_default(&identity_id);
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(&reply))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send default reply");
                            }
                        });
                        return;
                    }

                    // !context clear
                    if body.trim().eq_ignore_ascii_case("!context clear") {
                        ctx_store.clear(&chat_key);
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Err(e) = room
                                .send(RoomMessageEventContent::text_plain(
                                    "🧹 Conversation context cleared.",
                                ))
                                .await
                            {
                                warn!(error = %e, "Matrix: failed to send context-clear reply");
                            }
                        });
                        return;
                    }

                    // !approve / !deny — async approval commands delegated to CommandHandler.
                    if CommandHandler::is_approve_command(&body) || CommandHandler::is_deny_command(&body) {
                        debug!(sender = %sender, cmd = %body.trim(), "Matrix: handling async approval command");
                        let cmd = cmd_handler.clone();
                        let body_owned = body.clone();
                        let room = room.clone();
                        tokio::spawn(async move {
                            if let Some((ack, follow_up)) = cmd.handle_async(&body_owned).await {
                                let _ = room
                                    .send(RoomMessageEventContent::text_plain(&ack))
                                    .await;
                                if let Some(resp) = follow_up {
                                    // Try markdown for the continuation agent response; fall back to plain.
                                    let send_result = room
                                        .send(RoomMessageEventContent::text_markdown(&resp))
                                        .await;
                                    if let Err(e) = send_result {
                                        warn!(error = %e, "Matrix: markdown send failed for approval follow-up, retrying plain");
                                        let _ = room
                                            .send(RoomMessageEventContent::text_plain(&resp))
                                            .await;
                                    }
                                }
                            }
                        });
                        return;
                    }

                    // --- Agent dispatch ---
                    let agent_id = match cmd_handler.active_agent_for(&identity_id) {
                        Some(id) => id,
                        None => {
                            warn!(sender = %sender, identity = %identity_id, "Matrix: no routing rule — dropping");
                            return;
                        }
                    };

                    let agent = match find_agent(&agent_id, &config) {
                        Some(a) => a.clone(),
                        None => {
                            warn!(agent_id = %agent_id, "Matrix: agent not found in config");
                            let room = room.clone();
                            tokio::spawn(async move {
                                let _ = room
                                    .send(RoomMessageEventContent::text_plain(
                                        "⚠️ Agent not configured.",
                                    ))
                                    .await;
                            });
                            return;
                        }
                    };

                    let sender_label = config
                        .identities
                        .iter()
                        .find(|i| i.id == identity_id)
                        .and_then(|i| i.display_name.as_deref())
                        .unwrap_or(&identity_id)
                        .to_string();

                    // Spawn agent dispatch — event handler returns immediately
                    tokio::spawn(async move {
                        let augmented = ctx_store.augment_message(&chat_key, &agent_id, &body);

                        let dispatch_start = std::time::Instant::now();
                        match router.dispatch_with_sender(&augmented, &agent, &config, Some(&identity_id)).await {
                            Ok(response) => {
                                let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                                cmd_handler.record_dispatch(latency_ms);
                                debug!(
                                    identity = %identity_id,
                                    agent_id = %agent_id,
                                    response_len = %response.len(),
                                    "Matrix: got agent response"
                                );

                                ctx_store.push(&chat_key, &sender_label, &body, &agent_id, &response);

                                // Send response — try markdown first, fall back to plain text
                                let send_result = room
                                    .send(RoomMessageEventContent::text_markdown(&response))
                                    .await;
                                if let Err(e) = send_result {
                                    warn!(error = %e, "Matrix: markdown send failed, retrying plain text");
                                    let _ = room
                                        .send(RoomMessageEventContent::text_plain(&response))
                                        .await;
                                }
                            }
                            Err(e) => {
                                // ── Clash approval flow ─────────────────────────────────────
                                // Check if the agent loop paused for human approval.
                                if let Some(approval) = e.downcast_ref::<crate::adapters::AdapterError>() {
                                    if let crate::adapters::AdapterError::ApprovalPending(req) = approval {
                                        let req = req.clone();
                                        debug!(
                                            request_id = %req.request_id,
                                            command = %req.command,
                                            "Matrix: clash approval request — forwarding to user"
                                        );
                                        // Register in command handler so !approve / !deny can find it.
                                        cmd_handler.register_pending_approval(
                                            crate::adapters::openclaw::PendingApprovalMeta {
                                                request_id: req.request_id.clone(),
                                                nzc_endpoint: agent.endpoint.clone(),
                                                nzc_auth_token: agent
                                                    .auth_token
                                                    .clone()
                                                    .unwrap_or_default(),
                                                summary: format!(
                                                    "🔒 Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                                                    req.command, req.reason, req.request_id
                                                ),
                                            },
                                        ).await;

                                        // Send the approval notification to the user.
                                        let notification = format!(
                                            "🔒 Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                                            req.command, req.reason, req.request_id
                                        );
                                        let _ = room
                                            .send(RoomMessageEventContent::text_plain(&notification))
                                            .await;
                                        return; // Don't send an error — we already notified.
                                    }
                                }
                                // ─────────────────────────────────────────────────────────────
                                warn!(identity = %identity_id, error = %e, "Matrix: agent dispatch failed");
                                let _ = room
                                    .send(RoomMessageEventContent::text_plain(&format!(
                                        "⚠️ Agent error: {}",
                                        e
                                    )))
                                    .await;
                            }
                        }
                    });
                }
            },
        );

        // --- Sync loop ---
        let sync_settings = SyncSettings::new().timeout(std::time::Duration::from_secs(30));
        client
            .sync_with_result_callback(sync_settings, |sync_result| async move {
                if let Err(e) = sync_result {
                    warn!(error = %e, "Matrix: sync error, retrying in 5s...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
                Ok::<LoopCtrl, matrix_sdk::Error>(LoopCtrl::Continue)
            })
            .await?;

        Ok(())
    }
}

#[cfg(feature = "channel-matrix")]
pub use inner::run;
