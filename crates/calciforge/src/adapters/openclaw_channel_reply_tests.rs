use super::*;

use crate::messages::OutboundMessage;
use crate::sync::Arc;
use axum::extract::State;
use proptest::prelude::*;

#[tokio::test]
async fn reply_server_acks_correlated_error_callbacks() {
    let router = ReplyRouter::new();
    let request_id = "req-error".to_string();
    let session_key = "calciforge:main:renee".to_string();
    let (tx, rx) = oneshot::channel::<ReplyResult>();
    router
        .insert(request_id.clone(), session_key.clone(), tx)
        .await;

    let state = ReplyServerState {
        router,
        auth_tokens: Arc::new(StdMutex::new(HashSet::new())),
    };

    let payload = ReplyPayload {
        session_key,
        request_id: Some(request_id),
        message: None,
        error: Some(
            "OpenClaw completed without a visible reply for this Calciforge request".into(),
        ),
        error_kind: Some("no_visible_reply".into()),
        no_visible_reply_reason: Some("no_reply_dispatched".into()),
        attachments: Vec::new(),
        channel: Some("signal".into()),
        to: None,
    };

    let (status, Json(ack)) = handle_reply(State(state), HeaderMap::new(), Json(payload)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(ack.ok);
    let err = rx
        .await
        .expect("reply sender should not be dropped")
        .expect_err("callback error should route to waiter as protocol error");
    assert!(
        err.contains("kind=no_visible_reply"),
        "error should include kind: {err}"
    );
    assert!(
        err.contains("reason=no_reply_dispatched"),
        "error should include reason: {err}"
    );
}

#[tokio::test]
async fn reply_server_rejects_contradictory_message_and_error_callback() {
    let router = ReplyRouter::new();
    let request_id = "req-contradictory".to_string();
    let session_key = "calciforge:main:renee".to_string();
    let (tx, rx) = oneshot::channel::<ReplyResult>();
    router
        .insert(request_id.clone(), session_key.clone(), tx)
        .await;

    let state = ReplyServerState {
        router,
        auth_tokens: Arc::new(StdMutex::new(HashSet::new())),
    };

    let payload = ReplyPayload {
        session_key,
        request_id: Some(request_id),
        message: Some("valid-looking reply".into()),
        error: Some("runtime also claimed failure".into()),
        error_kind: Some("ambiguous_contract".into()),
        no_visible_reply_reason: None,
        attachments: Vec::new(),
        channel: Some("signal".into()),
        to: None,
    };

    let (status, Json(ack)) = handle_reply(State(state), HeaderMap::new(), Json(payload)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(ack.ok);
    let err = rx
        .await
        .expect("reply sender should not be dropped")
        .expect_err("contradictory callback must be treated as protocol error");
    assert!(
        err.contains("both message and error"),
        "error should explain contradictory callback shape: {err}"
    );
}

#[tokio::test]
async fn reply_token_must_match_pending_legacy_callback() {
    let router = ReplyRouter::new();
    let request_id = "req-victim".to_string();
    let session_key = "calciforge:victim:alice".to_string();
    let (tx, mut rx) = oneshot::channel::<ReplyResult>();
    router
        .insert_with_auth(
            request_id.clone(),
            session_key.clone(),
            Some("reply-secret-a".to_string()),
            tx,
        )
        .await;

    let state = ReplyServerState {
        router,
        auth_tokens: Arc::new(StdMutex::new(HashSet::from([
            "reply-secret-a".to_string(),
            "reply-secret-b".to_string(),
        ]))),
    };

    let payload = ReplyPayload {
        session_key: session_key.clone(),
        request_id: None,
        message: Some("spoofed legacy reply".into()),
        error: None,
        error_kind: None,
        no_visible_reply_reason: None,
        attachments: Vec::new(),
        channel: None,
        to: None,
    };
    let mut attacker_headers = HeaderMap::new();
    attacker_headers.insert("authorization", "Bearer reply-secret-b".parse().unwrap());

    let (status, Json(ack)) =
        handle_reply(State(state.clone()), attacker_headers, Json(payload)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!ack.ok);
    assert!(
        rx.try_recv().is_err(),
        "wrong-token callback must not consume the pending request"
    );

    let payload = ReplyPayload {
        session_key,
        request_id: Some(request_id),
        message: Some("victim reply".into()),
        error: None,
        error_kind: None,
        no_visible_reply_reason: None,
        attachments: Vec::new(),
        channel: None,
        to: None,
    };
    let mut victim_headers = HeaderMap::new();
    victim_headers.insert("authorization", "Bearer reply-secret-a".parse().unwrap());

    let (status, Json(ack)) = handle_reply(State(state), victim_headers, Json(payload)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(ack.ok);
    assert_eq!(
        rx.await.unwrap().unwrap().render_text_fallback(),
        "victim reply"
    );
}

#[tokio::test]
async fn legacy_session_key_callback_is_ambiguous_for_overlapping_dispatches() {
    let router = ReplyRouter::new();
    let (first_tx, first_rx) = oneshot::channel::<ReplyResult>();
    let (second_tx, second_rx) = oneshot::channel::<ReplyResult>();
    let session_key = "calciforge:main:brian".to_string();

    router
        .insert("request-1".to_string(), session_key.clone(), first_tx)
        .await;
    router
        .insert("request-2".to_string(), session_key.clone(), second_tx)
        .await;

    assert!(
        router.take(&session_key).await.is_none(),
        "legacy sessionKey-only callback must fail closed once the session has overlapping requests"
    );

    let first = router
        .take("request-1")
        .await
        .expect("requestId correlation for first request should remain available");
    first
        .send(Ok(OutboundMessage::text("first")))
        .expect("first receiver should still be live");
    assert_eq!(
        first_rx.await.unwrap().unwrap().render_text_fallback(),
        "first"
    );

    let second = router
        .take("request-2")
        .await
        .expect("requestId correlation for second request should remain available");
    second
        .send(Ok(OutboundMessage::text("second")))
        .expect("second receiver should still be live");
    assert_eq!(
        second_rx.await.unwrap().unwrap().render_text_fallback(),
        "second"
    );
}

proptest! {
    #[test]
    fn callback_shape_rejects_any_nonempty_message_plus_error(
        message in "\\PC{1,128}",
        error in "\\PC{1,128}",
    ) {
        let payload = ReplyPayload {
            session_key: "calciforge:main:generated".to_string(),
            request_id: Some("req-generated".to_string()),
            message: Some(message),
            error: Some(error),
            error_kind: None,
            no_visible_reply_reason: None,
            attachments: Vec::new(),
            channel: None,
            to: None,
        };

        prop_assert!(
            payload.validate_shape().is_err(),
            "callback cannot be both success and failure"
        );
    }

    #[test]
    fn callback_shape_accepts_success_or_failure_but_not_both(
        text in "\\PC{1,128}",
        choose_error in any::<bool>(),
    ) {
        let payload = ReplyPayload {
            session_key: "calciforge:main:generated".to_string(),
            request_id: Some("req-generated".to_string()),
            message: (!choose_error).then(|| text.clone()),
            error: choose_error.then_some(text),
            error_kind: None,
            no_visible_reply_reason: None,
            attachments: Vec::new(),
            channel: None,
            to: None,
        };

        prop_assert!(
            payload.validate_shape().is_ok(),
            "callback with exactly one terminal state should be valid"
        );
    }
}
