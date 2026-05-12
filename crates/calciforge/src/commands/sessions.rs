use crate::messages::{ChoiceControl, ChoiceOption, OutboundMessage};

pub(super) fn valid_downstream_session_name(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub(super) fn active_sessions_message(agent_id: &str, sessions: Vec<String>) -> OutboundMessage {
    let sessions = sessions
        .into_iter()
        .filter(|session| valid_downstream_session_name(session))
        .collect::<Vec<_>>();

    if sessions.is_empty() {
        return OutboundMessage::text(format!(
            "ℹ️ No attachable sessions for '{}'.\n\nUse !switch {} to create a new session.",
            agent_id, agent_id
        ));
    }

    let session_list = sessions.join("\n  - ");
    let reply = format!(
        "🗂️  Active sessions for '{}':\n  - {}\n\nUse !switch {} <session> to attach to a specific session.",
        agent_id, session_list, agent_id
    );
    OutboundMessage::text(reply).with_control(ChoiceControl::new(
        "Attach to a session",
        sessions
            .into_iter()
            .map(|session| ChoiceOption::session(session.clone(), agent_id, session))
            .collect(),
    ))
}
