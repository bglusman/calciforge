//! Conversation context — cross-agent ring-buffer with per-agent watermarks.
//!
//! # Design
//!
//! Each chat has a [`ConversationContext`] holding up to `capacity` recent
//! [`Exchange`]s in a [`VecDeque`] ring buffer.  Exchanges are identified by a
//! monotonically-increasing sequence number (`seq`) so each agent can track the
//! last exchange it has already seen.
//!
//! When dispatching to an agent, call [`ContextStore::build_preamble`] to get a
//! formatted prefix containing only the exchanges the agent hasn't seen yet
//! (capped at `inject_depth`).  After a successful dispatch, call
//! [`ContextStore::push`] to record the exchange and advance the watermark for
//! that agent.
//!
//! # Thread safety
//!
//! [`ContextStore`] wraps everything in `Arc<Mutex<…>>` and is safe to share
//! across async tasks via `.clone()`.
//!
//! # Wire format
//!
//! The preamble injected at the front of the outgoing message looks like:
//!
//! ```text
//! [Recent context:
//! Brian: hello
//! librarian: Hi there! How can I help?
//! Brian: actually, can you help with this code?
//! custodian: Sure! Let me take a look.]
//!
//! <current message>
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single turn in the conversation: one user prompt + one agent response.
#[derive(Debug, Clone)]
pub struct Exchange {
    /// Monotonically increasing sequence number within this chat's buffer.
    pub seq: u64,
    /// Human-readable sender label (e.g. "Brian").
    pub sender_label: String,
    /// The user's message.
    pub prompt: String,
    /// The agent that answered.
    pub agent_id: String,
    /// The agent's response.
    pub response: String,
}

/// Per-chat conversation context: ring buffer + per-agent watermarks.
///
/// `watermarks[agent_id]` = the `seq` of the last exchange the agent has already
/// seen (i.e., was part of when it generated its response, or was injected into
/// its preamble on that call).  On first switch to a new agent, no watermark
/// exists, so the agent gets the full recent history it missed.
pub struct ConversationContext {
    /// Ordered exchanges, oldest first.  Bounded to `capacity`.
    exchanges: VecDeque<Exchange>,
    /// Maximum number of exchanges to retain.
    capacity: usize,
    /// Next sequence number to assign.
    next_seq: u64,
    /// Per-agent watermark: last `seq` seen (inclusive).
    watermarks: HashMap<String, u64>,
}

impl ConversationContext {
    pub fn new(capacity: usize) -> Self {
        Self {
            exchanges: VecDeque::with_capacity(capacity),
            capacity,
            next_seq: 0,
            watermarks: HashMap::new(),
        }
    }

    /// Push a completed exchange and advance the watermark for `agent_id`.
    ///
    /// The agent that just answered is considered to have seen this exchange
    /// (it generated the response), so its watermark advances to `seq`.
    pub fn push(&mut self, sender_label: String, prompt: String, agent_id: String, response: String) {
        let seq = self.next_seq;
        self.next_seq += 1;

        if self.exchanges.len() == self.capacity {
            self.exchanges.pop_front();
        }
        self.exchanges.push_back(Exchange {
            seq,
            sender_label,
            prompt,
            agent_id: agent_id.clone(),
            response,
        });

        // Advance this agent's watermark — it generated the response, so it
        // "saw" this exchange.
        self.watermarks.insert(agent_id, seq);
    }

    /// Build a context preamble for `agent_id`, injecting up to `inject_depth`
    /// exchanges that the agent has NOT yet seen.
    ///
    /// Returns `None` if there are no unseen exchanges (no preamble needed).
    pub fn build_preamble(&self, agent_id: &str, inject_depth: usize) -> Option<String> {
        if inject_depth == 0 || self.exchanges.is_empty() {
            return None;
        }

        // Exchanges the agent hasn't seen: seq > watermark (or all if no watermark)
        let last_seen = self.watermarks.get(agent_id).copied();

        let unseen: Vec<&Exchange> = self
            .exchanges
            .iter()
            .filter(|e| match last_seen {
                None => true,
                Some(wm) => e.seq > wm,
            })
            .collect();

        if unseen.is_empty() {
            return None;
        }

        // Take at most `inject_depth` most-recent unseen exchanges.
        let to_inject = if unseen.len() > inject_depth {
            &unseen[unseen.len() - inject_depth..]
        } else {
            &unseen[..]
        };

        let mut lines = vec!["[Recent context:".to_string()];
        for ex in to_inject {
            lines.push(format!("{}: {}", ex.sender_label, ex.prompt));
            lines.push(format!("{}: {}", ex.agent_id, ex.response));
        }
        lines.push("]".to_string());

        Some(lines.join("\n"))
    }

    /// Advance the watermark for `agent_id` to the latest exchange without
    /// pushing a new exchange.  Call this after injecting context so the agent
    /// is not re-sent the same history on the next turn (if it answers that turn).
    ///
    /// In practice this is handled automatically by [`push`] — this method is
    /// provided as an escape hatch for tests or future channels.
    pub fn mark_seen(&mut self, agent_id: &str) {
        if let Some(last) = self.exchanges.back() {
            self.watermarks.insert(agent_id.to_string(), last.seq);
        }
    }

    /// Number of exchanges currently in the buffer.
    pub fn len(&self) -> usize {
        self.exchanges.len()
    }

    /// True if no exchanges have been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }
}

// ---------------------------------------------------------------------------
// ContextStore — shared across async tasks
// ---------------------------------------------------------------------------

/// Thread-safe store mapping `chat_id` (string) → [`ConversationContext`].
///
/// Clone is cheap — the inner `Arc` is reference-counted.
#[derive(Clone)]
pub struct ContextStore {
    inner: Arc<Mutex<HashMap<String, ConversationContext>>>,
    buffer_size: usize,
    inject_depth: usize,
}

impl ContextStore {
    /// Create a new store.
    ///
    /// - `buffer_size`: max exchanges per chat (ring buffer capacity)
    /// - `inject_depth`: max unseen exchanges to prepend on each dispatch
    pub fn new(buffer_size: usize, inject_depth: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            buffer_size,
            inject_depth,
        }
    }

    /// Build a context preamble for a chat+agent pair and return the full
    /// message to send (preamble prepended if non-empty).
    ///
    /// After calling this, the agent's watermark is NOT yet advanced — call
    /// [`push`] after a successful response to do that.
    pub fn augment_message(&self, chat_id: &str, agent_id: &str, message: &str) -> String {
        let map = self.inner.lock().unwrap();
        let preamble = map
            .get(chat_id)
            .and_then(|ctx| ctx.build_preamble(agent_id, self.inject_depth));

        match preamble {
            None => message.to_string(),
            Some(pre) => format!("{}\n\n{}", pre, message),
        }
    }

    /// Record a completed exchange and advance the agent's watermark.
    pub fn push(
        &self,
        chat_id: &str,
        sender_label: &str,
        prompt: &str,
        agent_id: &str,
        response: &str,
    ) {
        let mut map = self.inner.lock().unwrap();
        let ctx = map
            .entry(chat_id.to_string())
            .or_insert_with(|| ConversationContext::new(self.buffer_size));
        ctx.push(
            sender_label.to_string(),
            prompt.to_string(),
            agent_id.to_string(),
            response.to_string(),
        );
    }

    /// Clear the conversation context for a chat (e.g. `!context clear`).
    pub fn clear(&self, chat_id: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(chat_id);
    }

    /// Return the number of exchanges stored for a chat (for status/debug).
    pub fn exchange_count(&self, chat_id: &str) -> usize {
        let map = self.inner.lock().unwrap();
        map.get(chat_id).map(|c| c.len()).unwrap_or(0)
    }

    /// Configured inject depth.
    pub fn inject_depth(&self) -> usize {
        self.inject_depth
    }

    /// Configured buffer size.
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ConversationContext unit tests
    // -----------------------------------------------------------------------

    fn push_exchange(ctx: &mut ConversationContext, sender: &str, prompt: &str, agent: &str, response: &str) {
        ctx.push(sender.to_string(), prompt.to_string(), agent.to_string(), response.to_string());
    }

    #[test]
    fn test_empty_context_no_preamble() {
        let ctx = ConversationContext::new(20);
        assert!(ctx.build_preamble("librarian", 5).is_none());
    }

    #[test]
    fn test_push_increments_len() {
        let mut ctx = ConversationContext::new(20);
        assert_eq!(ctx.len(), 0);
        push_exchange(&mut ctx, "Brian", "hello", "librarian", "hi");
        assert_eq!(ctx.len(), 1);
        push_exchange(&mut ctx, "Brian", "bye", "librarian", "goodbye");
        assert_eq!(ctx.len(), 2);
    }

    #[test]
    fn test_ring_buffer_caps_at_capacity() {
        let mut ctx = ConversationContext::new(3);
        for i in 0..5 {
            push_exchange(&mut ctx, "Brian", &format!("msg {}", i), "librarian", &format!("resp {}", i));
        }
        assert_eq!(ctx.len(), 3, "should cap at capacity=3");
    }

    #[test]
    fn test_agent_that_generated_response_sees_no_new_preamble() {
        // librarian answers — it should not get its own exchange injected back
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "hello", "librarian", "hi there");
        // librarian's watermark was advanced by push()
        assert!(
            ctx.build_preamble("librarian", 5).is_none(),
            "agent that answered should have no unseen exchanges"
        );
    }

    #[test]
    fn test_new_agent_sees_all_prior_exchanges() {
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "hello", "librarian", "hi");
        push_exchange(&mut ctx, "Brian", "how are you?", "librarian", "good thanks");

        // custodian has never seen any of these
        let pre = ctx.build_preamble("custodian", 5).expect("should have preamble");
        assert!(pre.contains("hello"), "should contain first prompt: {}", pre);
        assert!(pre.contains("hi"), "should contain first response: {}", pre);
        assert!(pre.contains("how are you?"), "should contain second prompt: {}", pre);
    }

    #[test]
    fn test_inject_depth_limits_preamble_length() {
        let mut ctx = ConversationContext::new(20);
        for i in 0..10 {
            push_exchange(&mut ctx, "Brian", &format!("msg {}", i), "librarian", &format!("resp {}", i));
        }
        // custodian has seen none — inject_depth=3 should only give last 3
        let pre = ctx.build_preamble("custodian", 3).expect("should have preamble");
        // Last 3 exchanges: msg 7, msg 8, msg 9
        assert!(pre.contains("msg 9"), "should contain most recent: {}", pre);
        assert!(pre.contains("msg 8"), "should contain second recent: {}", pre);
        assert!(pre.contains("msg 7"), "should contain third recent: {}", pre);
        assert!(!pre.contains("msg 6"), "should NOT contain older than depth: {}", pre);
    }

    #[test]
    fn test_preamble_format() {
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "what is 2+2?", "librarian", "It's 4.");

        let pre = ctx.build_preamble("custodian", 5).unwrap();
        assert!(pre.starts_with("[Recent context:"), "should start with header: {}", pre);
        assert!(pre.contains("Brian: what is 2+2?"), "should have sender label: {}", pre);
        assert!(pre.contains("librarian: It's 4."), "should have agent label: {}", pre);
        assert!(pre.ends_with(']'), "should end with closing bracket: {}", pre);
    }

    #[test]
    fn test_watermark_advances_after_answer() {
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "q1", "librarian", "a1");
        push_exchange(&mut ctx, "Brian", "q2", "custodian", "a2");

        // librarian answered q1 (seq=0) but NOT q2 (seq=1, answered by custodian)
        // so librarian should see q2
        let pre = ctx.build_preamble("librarian", 5).expect("librarian should see custodian's exchange");
        assert!(pre.contains("q2"), "librarian should see the custodian exchange: {}", pre);
        assert!(!pre.contains("q1"), "librarian should NOT see its own exchange: {}", pre);
    }

    #[test]
    fn test_inject_depth_zero_returns_none() {
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "hello", "librarian", "hi");
        assert!(ctx.build_preamble("custodian", 0).is_none());
    }

    #[test]
    fn test_mark_seen_suppresses_preamble() {
        let mut ctx = ConversationContext::new(20);
        push_exchange(&mut ctx, "Brian", "hello", "librarian", "hi");
        // Manually mark custodian as having seen everything
        ctx.mark_seen("custodian");
        assert!(ctx.build_preamble("custodian", 5).is_none());
    }

    // -----------------------------------------------------------------------
    // ContextStore integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_store_augment_no_history() {
        let store = ContextStore::new(20, 5);
        let msg = store.augment_message("chat:1", "librarian", "hello agent");
        assert_eq!(msg, "hello agent", "no context = passthrough: {}", msg);
    }

    #[test]
    fn test_store_augment_prepends_preamble() {
        let store = ContextStore::new(20, 5);
        store.push("chat:1", "Brian", "first message", "librarian", "first reply");

        // custodian hasn't seen anything yet
        let msg = store.augment_message("chat:1", "custodian", "second message");
        assert!(msg.starts_with("[Recent context:"), "should prepend preamble: {}", msg);
        assert!(msg.contains("first message"), "should include prior exchange: {}", msg);
        assert!(msg.ends_with("second message"), "original message should be at the end: {}", msg);
    }

    #[test]
    fn test_store_augment_same_agent_no_preamble() {
        let store = ContextStore::new(20, 5);
        store.push("chat:1", "Brian", "first message", "librarian", "first reply");
        // librarian answered, so its watermark is up to date
        let msg = store.augment_message("chat:1", "librarian", "second message");
        assert_eq!(msg, "second message", "same agent should get no preamble: {}", msg);
    }

    #[test]
    fn test_store_push_increments_count() {
        let store = ContextStore::new(20, 5);
        assert_eq!(store.exchange_count("chat:1"), 0);
        store.push("chat:1", "Brian", "hi", "librarian", "hello");
        assert_eq!(store.exchange_count("chat:1"), 1);
        store.push("chat:1", "Brian", "bye", "librarian", "goodbye");
        assert_eq!(store.exchange_count("chat:1"), 2);
    }

    #[test]
    fn test_store_clear_removes_history() {
        let store = ContextStore::new(20, 5);
        store.push("chat:1", "Brian", "hi", "librarian", "hello");
        assert_eq!(store.exchange_count("chat:1"), 1);
        store.clear("chat:1");
        assert_eq!(store.exchange_count("chat:1"), 0);
    }

    #[test]
    fn test_store_independent_per_chat() {
        let store = ContextStore::new(20, 5);
        store.push("chat:1", "Brian", "chat1 msg", "librarian", "chat1 reply");
        // chat:2 should be isolated
        assert_eq!(store.exchange_count("chat:2"), 0);
        let msg = store.augment_message("chat:2", "custodian", "fresh start");
        assert_eq!(msg, "fresh start");
    }

    #[test]
    fn test_store_inject_depth_respected() {
        let store = ContextStore::new(20, 2); // inject_depth=2
        for i in 0..5 {
            store.push("chat:1", "Brian", &format!("q{}", i), "librarian", &format!("a{}", i));
        }
        // custodian hasn't seen any — but inject_depth=2 limits to last 2
        let msg = store.augment_message("chat:1", "custodian", "new question");
        assert!(msg.contains("q4"), "should have most recent: {}", msg);
        assert!(msg.contains("q3"), "should have second most recent: {}", msg);
        assert!(!msg.contains("q2"), "should NOT have older than depth=2: {}", msg);
    }

    #[test]
    fn test_store_clone_shares_state() {
        let store = ContextStore::new(20, 5);
        let store2 = store.clone();
        store.push("chat:1", "Brian", "hi", "librarian", "hello");
        // clone sees the same data
        assert_eq!(store2.exchange_count("chat:1"), 1);
    }

    #[test]
    fn test_preamble_separator_between_preamble_and_message() {
        let store = ContextStore::new(20, 5);
        store.push("chat:1", "Brian", "old message", "librarian", "old reply");
        let msg = store.augment_message("chat:1", "custodian", "new message");
        // Should have blank line between ']' and the message
        assert!(msg.contains("]\n\nnew message"), "should have blank line separator: {:?}", msg);
    }
}
