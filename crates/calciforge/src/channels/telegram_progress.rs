use std::time::{Duration, Instant};

use teloxide::{Bot, prelude::Requester, types::ChatId};
use tokio::sync::oneshot;

use super::telemetry;

pub const SLOW_AGENT_NOTICE_AFTER: Duration = Duration::from_secs(15);

pub fn spawn_slow_agent_notice(bot: Bot, chat_id: ChatId, agent_id: String) -> oneshot::Sender<()> {
    let (dispatch_done_tx, dispatch_done_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(SLOW_AGENT_NOTICE_AFTER) => {
                send_slow_notice(bot, chat_id, &agent_id).await;
            }
            _ = dispatch_done_rx => {}
        }
    });
    dispatch_done_tx
}

async fn send_slow_notice(bot: Bot, chat_id: ChatId, agent_id: &str) {
    let reply = slow_agent_notice(agent_id);
    let response_len = reply.len();
    let start = Instant::now();
    match bot.send_message(chat_id, reply).await {
        Ok(_) => telemetry::reply_sent(
            "telegram",
            &chat_id.to_string(),
            "agent_slow_notice",
            response_len,
            start.elapsed().as_millis() as u64,
        ),
        Err(e) => telemetry::reply_failed(
            "telegram",
            &chat_id.to_string(),
            "agent_slow_notice",
            start.elapsed().as_millis() as u64,
            e,
        ),
    }
}

fn slow_agent_notice(agent_id: &str) -> String {
    format!(
        "Still working on {agent_id}. This agent can take a while; I will send the reply here when it finishes."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_agent_notice_names_agent_and_final_delivery() {
        let notice = slow_agent_notice("openclaw-local");
        assert!(notice.contains("openclaw-local"));
        assert!(
            notice.contains("send the reply here when it finishes"),
            "notice should make clear this is progress, not a terminal error: {notice}"
        );
    }
}
