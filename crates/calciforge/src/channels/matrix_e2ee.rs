//! Matrix SDK-backed runtime for encrypted rooms.
//!
//! The raw HTTP Matrix adapter cannot decrypt `m.room.encrypted` events and
//! cannot encrypt outbound sends. This module is only compiled when
//! `channel-matrix-e2ee` is enabled and owns the SDK path for rooms where E2EE
//! is required.

use std::{
    collections::{HashSet, VecDeque},
    path::Path,
    time::Duration,
};

use anyhow::{Context as _, Result};
use matrix_sdk::{
    Client, Room, SessionMeta, SessionTokens,
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    ruma::{
        OwnedDeviceId, OwnedRoomId, OwnedUserId,
        events::room::message::{RoomMessageEventContent, SyncRoomMessageEvent},
    },
};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::sync::Arc;
use crate::{
    auth::resolve_channel_sender,
    commands::CommandHandler,
    config::{CalciforgeConfig, expand_tilde},
    context::ContextStore,
    messages::OutboundMessage,
    router::Router,
};

use super::{
    matrix::{MatrixSendOutbound, MatrixSendText, cache_event_id, handle_message_with_senders},
    telemetry,
};

pub(super) struct MatrixSdkRuntime {
    pub(super) config: Arc<CalciforgeConfig>,
    pub(super) router: Arc<Router>,
    pub(super) command_handler: Arc<CommandHandler>,
    pub(super) context_store: ContextStore,
    pub(super) homeserver: String,
    pub(super) access_token: String,
    pub(super) user_id: String,
    pub(super) device_id: Option<String>,
    pub(super) target_room: String,
    pub(super) allowed_users: Vec<String>,
    pub(super) store_path: String,
    pub(super) store_passphrase_file: Option<String>,
}

#[derive(Debug, Default)]
struct MatrixSdkDedup {
    order: VecDeque<String>,
    lookup: HashSet<String>,
}

impl MatrixSdkDedup {
    fn cache(&mut self, event_id: &str) -> bool {
        cache_event_id(event_id, &mut self.order, &mut self.lookup)
    }
}

pub fn e2ee_client_builder(
    homeserver: &str,
    store_path: impl AsRef<Path>,
    store_passphrase: Option<&str>,
) -> matrix_sdk::ClientBuilder {
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(store_path, store_passphrase)
}

pub(super) async fn run_sdk_runtime(runtime: MatrixSdkRuntime) -> Result<()> {
    let store_passphrase = read_store_passphrase(runtime.store_passphrase_file.as_deref()).await?;
    let client = e2ee_client_builder(
        &runtime.homeserver,
        expand_tilde(&runtime.store_path),
        store_passphrase.as_deref(),
    )
    .build()
    .await
    .context("Matrix SDK: failed to build E2EE client")?;

    let session = matrix_session(
        &runtime.user_id,
        runtime.device_id.as_deref(),
        runtime.access_token,
    )?;
    client
        .restore_session(session)
        .await
        .context("Matrix SDK: failed to restore access-token session")?;

    info!("Matrix SDK: performing initial sync to populate crypto and room state");
    client
        .sync_once(SyncSettings::new().timeout(Duration::from_secs(0)))
        .await
        .context("Matrix SDK: initial sync failed")?;

    let room_id = runtime
        .target_room
        .parse::<OwnedRoomId>()
        .with_context(|| format!("Matrix SDK: invalid room_id '{}'", runtime.target_room))?;
    let room = client
        .get_room(&room_id)
        .with_context(|| format!("Matrix SDK: configured room '{}' was not joined", room_id))?;
    let encryption_state = room
        .latest_encryption_state()
        .await
        .context("Matrix SDK: failed to confirm room encryption state")?;
    if !encryption_state.is_encrypted() {
        anyhow::bail!(
            "Matrix SDK: configured room '{}' is not encrypted after SDK state sync",
            room_id
        );
    }

    let dedup = Arc::new(Mutex::new(MatrixSdkDedup::default()));
    let target_room = room_id.to_string();
    let user_id = runtime.user_id.clone();
    let allowed_users = runtime.allowed_users.clone();
    let config = runtime.config.clone();
    let router = runtime.router.clone();
    let command_handler = runtime.command_handler.clone();
    let context_store = runtime.context_store.clone();

    client.add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
        let dedup = dedup.clone();
        let target_room = target_room.clone();
        let user_id = user_id.clone();
        let allowed_users = allowed_users.clone();
        let config = config.clone();
        let router = router.clone();
        let command_handler = command_handler.clone();
        let context_store = context_store.clone();

        async move {
            if room.room_id().as_str() != target_room {
                return;
            }

            let SyncRoomMessageEvent::Original(event) = event else {
                return;
            };

            let sender = event.sender.to_string();
            if sender == user_id {
                return;
            }
            if !super::matrix::is_sender_allowed(&allowed_users, &sender) {
                debug!(sender = %sender, "Matrix SDK: dropping message from non-allowed user");
                return;
            }

            let msgtype = event.content.msgtype();
            if msgtype != "m.text" && msgtype != "m.notice" {
                return;
            }
            let body = event.content.body().to_string();
            if body.trim().is_empty() {
                return;
            }

            let event_id = event.event_id.to_string();
            let mut dedup_guard = dedup.lock().await;
            if dedup_guard.cache(&event_id) {
                debug!(event_id = %event_id, "Matrix SDK: duplicate event, skipping");
                return;
            }
            drop(dedup_guard);

            let identity = resolve_channel_sender("matrix", &sender, &config);
            let identity_id = identity
                .as_ref()
                .map(|i| i.id.clone())
                .unwrap_or_else(|| sender.clone());
            let chat_key = format!("matrix-{}", identity_id);
            telemetry::authorized_message("matrix", &identity_id, &sender, body.len(), None);

            let received_at = std::time::Instant::now();
            tokio::spawn(async move {
                let send = sdk_text_sender(room.clone());
                let send_outbound = sdk_outbound_sender(room);
                handle_message_with_senders(
                    &sender,
                    &identity_id,
                    &chat_key,
                    &body,
                    &config,
                    &router,
                    &command_handler,
                    &context_store,
                    received_at,
                    send,
                    send_outbound,
                )
                .await;
            });
        }
    });

    info!(
        room_id = %room_id,
        user_id = %runtime.user_id,
        allowed_users = ?runtime.allowed_users,
        "Matrix SDK E2EE channel listening"
    );

    let mut retry_delay_secs = 5u64;
    loop {
        match client.sync(SyncSettings::default()).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!(
                    error = %error,
                    retry_delay_secs,
                    "Matrix SDK: sync loop error, retrying"
                );
                tokio::time::sleep(Duration::from_secs(retry_delay_secs)).await;
                retry_delay_secs = (retry_delay_secs * 2).min(60);
            }
        }
    }
}

fn matrix_session(
    user_id: &str,
    device_id: Option<&str>,
    access_token: String,
) -> Result<MatrixSession> {
    let user_id = user_id
        .parse::<OwnedUserId>()
        .with_context(|| format!("Matrix SDK: invalid user_id '{user_id}'"))?;
    let device_id = device_id.context(
        "Matrix SDK: /account/whoami did not return device_id; E2EE session restoration requires a device-bound access token",
    )?;
    let device_id: OwnedDeviceId = device_id.into();

    Ok(MatrixSession {
        meta: SessionMeta { user_id, device_id },
        tokens: SessionTokens {
            access_token,
            refresh_token: None,
        },
    })
}

async fn read_store_passphrase(path: Option<&str>) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let passphrase = tokio::fs::read_to_string(expand_tilde(path))
        .await
        .with_context(|| {
            format!("Matrix SDK: failed to read matrix_e2ee_store_passphrase_file '{path}'")
        })?
        .trim()
        .to_string();
    Ok((!passphrase.is_empty()).then_some(passphrase))
}

fn sdk_text_sender(room: Room) -> MatrixSendText {
    Arc::new(move |text: String, reply_kind: &'static str| {
        let room = room.clone();
        Box::pin(async move {
            let start = std::time::Instant::now();
            let room_id = room.room_id().to_string();
            let response_len = text.len();
            match room.send(RoomMessageEventContent::text_plain(text)).await {
                Ok(_) => telemetry::reply_sent(
                    "matrix",
                    &room_id,
                    reply_kind,
                    response_len,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => telemetry::reply_failed(
                    "matrix",
                    &room_id,
                    reply_kind,
                    start.elapsed().as_millis() as u64,
                    e,
                ),
            }
        })
    })
}

fn sdk_outbound_sender(room: Room) -> MatrixSendOutbound {
    Arc::new(move |message: OutboundMessage, reply_kind: &'static str| {
        let room = room.clone();
        Box::pin(async move {
            let start = std::time::Instant::now();
            let room_id = room.room_id().to_string();
            let response_len = message.response_len();
            if !message.attachments.is_empty() {
                warn!(
                    attachments = message.attachments.len(),
                    "Matrix SDK: encrypted native media upload is not implemented; sending encrypted text fallback"
                );
            }
            match room
                .send(RoomMessageEventContent::text_plain(
                    message.render_text_fallback(),
                ))
                .await
            {
                Ok(_) => telemetry::reply_sent(
                    "matrix",
                    &room_id,
                    reply_kind,
                    response_len,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => telemetry::reply_failed(
                    "matrix",
                    &room_id,
                    reply_kind,
                    start.elapsed().as_millis() as u64,
                    e,
                ),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn e2ee_builder_accepts_persistent_sqlite_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let client = e2ee_client_builder(
            "https://matrix.example.test",
            temp.path().join("matrix-sdk-store"),
            Some("test-passphrase"),
        )
        .build()
        .await
        .expect("SDK client should build with persistent E2EE store");

        let _encryption = client.encryption();
        assert_eq!(client.homeserver().as_str(), "https://matrix.example.test/");
    }

    #[test]
    fn matrix_session_requires_device_id_for_e2ee_restore() {
        let err = matrix_session("@bot:example.test", None, "token".to_string())
            .expect_err("device_id is required for SDK E2EE session restoration");
        assert!(err.to_string().contains("device_id"));
    }

    #[test]
    fn matrix_session_accepts_device_bound_access_token() {
        let session = matrix_session(
            "@bot:example.test",
            Some("CALCIFORGEDEVICE"),
            "token".to_string(),
        )
        .expect("valid Matrix session");
        assert_eq!(session.meta.user_id.as_str(), "@bot:example.test");
        assert_eq!(session.meta.device_id.as_str(), "CALCIFORGEDEVICE");
        assert_eq!(session.tokens.access_token, "token");
    }
}
