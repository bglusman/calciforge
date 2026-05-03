//! Per-identity pending-choice tracker for inbound-reply resolution.
//!
//! When a channel sends an `OutboundMessage` whose `controls` is
//! non-empty, the channel records the pending choices here keyed by
//! `(channel_kind, identity-or-recipient)`. When the user's next
//! inbound message arrives, the channel queries this state and tries
//! to map the reply onto a specific `ChoiceOption`. Two resolution
//! paths:
//!
//!   1. `[choice]<id-or-title>` sentinel — produced by the
//!      zeroclawlabs fork's native-interactive deserializers
//!      (Signal `pollAnswer.selected_titles` and WhatsApp
//!      `interactive.button_reply.id` / `list_reply.id`). Resolved
//!      via [`ChoiceState::resolve_sentinel`] which looks up the
//!      option by `callback_data` (preferred) or `label` (fallback).
//!
//!   2. Free-text reply ("2", "Librarian", "lib") — produced by the
//!      user typing on text-fallback channels (Matrix, SMS, mock,
//!      and any channel whose `send_choice` falls through to the
//!      default text rendering). Resolved via
//!      [`ChoiceState::match_reply`] which delegates to
//!      [`ChoiceControl::match_reply`].
//!
//! Pending state expires after a configurable TTL (default 5 minutes)
//! so a stale poll the user ignores doesn't intercept later freeform
//! input. Sending a fresh `OutboundMessage` to the same identity
//! replaces any prior pending state; sending a message WITHOUT
//! controls clears it.


use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::messages::{ChoiceControl, ChoiceOption, Match};

const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
struct Pending {
    controls: Vec<ChoiceControl>,
    expires_at: Instant,
}

/// Thread-safe, identity-keyed store of pending choices.
///
/// Constructed once at daemon startup, shared (`Arc<ChoiceState>`)
/// across every channel's `run()`.
#[derive(Debug)]
pub struct ChoiceState {
    /// Key: `(channel_kind, identity-or-recipient-key)`.
    /// Channels that resolve identity at send time should pass the
    /// stable identity-id (so DM and group sends from the same person
    /// share state); otherwise the recipient string (e.g. E.164 phone)
    /// is acceptable. The key just needs to match between
    /// `record` (outbound) and the resolution methods (inbound).
    inner: Mutex<HashMap<(String, String), Pending>>,
    ttl: Duration,
}

impl Default for ChoiceState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChoiceState {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Record pending controls for this identity. Replaces any prior
    /// pending state for the same key. Empty `controls` clears.
    pub fn record(&self, channel_kind: &str, key: &str, controls: Vec<ChoiceControl>) {
        let mut map = self.inner.lock().unwrap();
        let entry_key = (channel_kind.to_string(), key.to_string());
        if controls.is_empty() {
            map.remove(&entry_key);
            return;
        }
        map.insert(
            entry_key,
            Pending {
                controls,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    /// Try to resolve free-text `reply` against the pending control(s).
    /// Side effect: removes the matched pending state on success.
    pub fn match_reply(&self, channel_kind: &str, key: &str, reply: &str) -> ChoiceMatchResult {
        let mut map = self.inner.lock().unwrap();
        let entry_key = (channel_kind.to_string(), key.to_string());
        let Some(pending) = map.get(&entry_key) else {
            return ChoiceMatchResult::NoPending;
        };
        if Instant::now() >= pending.expires_at {
            map.remove(&entry_key);
            return ChoiceMatchResult::Expired;
        }
        // Try each control in order; first definitive verdict wins.
        // Common case: a single control.
        let mut last_neg = ChoiceMatchResult::NoMatch;
        for control in &pending.controls {
            match control.match_reply(reply) {
                Match::One(option) => {
                    let dispatched = ChoiceMatchResult::Match {
                        command: option.command.clone(),
                        callback_data: option.callback_data.clone(),
                        label: option.label.clone(),
                    };
                    map.remove(&entry_key);
                    return dispatched;
                }
                Match::Ambiguous => return ChoiceMatchResult::Ambiguous,
                Match::OutOfRange => last_neg = ChoiceMatchResult::OutOfRange,
                Match::None => {}
            }
        }
        last_neg
    }

    /// Resolve a `[choice]<id-or-title>` sentinel from a native
    /// interactive event (Signal poll-vote, WhatsApp interactive
    /// reply). Looks up by `callback_data` first, then `label`
    /// (case-insensitive). Side effect: removes the matched pending
    /// state on success.
    pub fn resolve_sentinel(
        &self,
        channel_kind: &str,
        key: &str,
        id_or_title: &str,
    ) -> Option<ResolvedOption> {
        let mut map = self.inner.lock().unwrap();
        let entry_key = (channel_kind.to_string(), key.to_string());
        let pending = map.get(&entry_key)?;
        if Instant::now() >= pending.expires_at {
            map.remove(&entry_key);
            return None;
        }
        for control in &pending.controls {
            if let Some(opt) = control_lookup(&control.options, id_or_title) {
                let resolved = ResolvedOption::from(opt);
                map.remove(&entry_key);
                return Some(resolved);
            }
        }
        None
    }

    /// Drop expired entries. Cheap; safe to call from a periodic
    /// maintenance task.
    #[allow(dead_code)]
    pub fn evict_expired(&self) {
        let now = Instant::now();
        self.inner.lock().unwrap().retain(|_, p| p.expires_at > now);
    }

    /// Strictly for tests / observability.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn pending_len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

fn control_lookup<'a>(options: &'a [ChoiceOption], id_or_title: &str) -> Option<&'a ChoiceOption> {
    if let Some(opt) = options
        .iter()
        .find(|o| o.callback_data.as_deref() == Some(id_or_title))
    {
        return Some(opt);
    }
    options
        .iter()
        .find(|o| o.label.eq_ignore_ascii_case(id_or_title))
}

/// Resolved option payload returned to the channel for dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOption {
    pub command: String,
    pub callback_data: Option<String>,
    pub label: String,
}

impl From<&ChoiceOption> for ResolvedOption {
    fn from(o: &ChoiceOption) -> Self {
        Self {
            command: o.command.clone(),
            callback_data: o.callback_data.clone(),
            label: o.label.clone(),
        }
    }
}

/// Outcome of resolving a free-text reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceMatchResult {
    /// Reply unambiguously matched an option — dispatch the command.
    Match {
        command: String,
        callback_data: Option<String>,
        label: String,
    },
    /// Reply matched more than one option — channel should re-prompt.
    Ambiguous,
    /// Reply parsed as a number outside [1, N] — channel may re-prompt
    /// with the valid range.
    OutOfRange,
    /// Reply doesn't look like a selection — channel should treat it
    /// as freeform input.
    NoMatch,
    /// No pending state for this key.
    NoPending,
    /// Pending state existed but has expired (state was removed).
    Expired,
}

/// Sentinel prefix the native-interactive deserializers prepend.
/// Channels that produce these (in their inbound message handler)
/// should strip the prefix and call [`ChoiceState::resolve_sentinel`]
/// with the remainder.
pub const CHOICE_SENTINEL_PREFIX: &str = "[choice]";
/// Fallback sentinel for poll votes when only the index is available.
pub const CHOICE_INDEX_SENTINEL_PREFIX: &str = "[choice-index]";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ChoiceControl, ChoiceOption};

    fn ctrl_two() -> ChoiceControl {
        ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        )
    }

    #[test]
    fn record_then_match_by_number() {
        let state = ChoiceState::new();
        state.record("signal", "+15555550100", vec![ctrl_two()]);
        let result = state.match_reply("signal", "+15555550100", "1");
        match result {
            ChoiceMatchResult::Match { ref command, .. } => {
                assert!(command.contains("librarian"));
            }
            other => panic!("expected Match, got {other:?}"),
        }
        // Pending state cleared on match.
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn record_then_match_by_label() {
        let state = ChoiceState::new();
        state.record("signal", "+15555550100", vec![ctrl_two()]);
        let result = state.match_reply("signal", "+15555550100", "critic");
        match result {
            ChoiceMatchResult::Match { ref command, .. } => {
                assert!(command.contains("critic"));
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn empty_controls_clears_state() {
        let state = ChoiceState::new();
        state.record("signal", "+15555550100", vec![ctrl_two()]);
        assert_eq!(state.pending_len(), 1);
        state.record("signal", "+15555550100", vec![]);
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn unrelated_key_returns_no_pending() {
        let state = ChoiceState::new();
        state.record("signal", "+15555550100", vec![ctrl_two()]);
        let result = state.match_reply("signal", "+15555550199", "1");
        assert_eq!(result, ChoiceMatchResult::NoPending);
    }

    #[test]
    fn cross_channel_keys_dont_collide() {
        let state = ChoiceState::new();
        state.record("signal", "alice", vec![ctrl_two()]);
        state.record("whatsapp", "alice", vec![ctrl_two()]);
        // Match on signal — whatsapp pending stays.
        let _ = state.match_reply("signal", "alice", "1");
        assert_eq!(state.pending_len(), 1);
        let result = state.match_reply("whatsapp", "alice", "2");
        assert!(matches!(result, ChoiceMatchResult::Match { .. }));
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn expired_entry_returns_expired_and_evicts() {
        let state = ChoiceState::with_ttl(Duration::from_millis(0));
        state.record("signal", "alice", vec![ctrl_two()]);
        // TTL=0 → already expired. Sleep a beat to avoid the rare
        // case of `Instant::now()` being identical to `expires_at`.
        std::thread::sleep(Duration::from_millis(1));
        let result = state.match_reply("signal", "alice", "1");
        assert_eq!(result, ChoiceMatchResult::Expired);
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn resolve_sentinel_by_callback_data() {
        let state = ChoiceState::new();
        let ctrl = ctrl_two();
        // ChoiceOption::agent("Librarian", "librarian") sets
        // callback_data = "cf:agent:librarian"
        state.record("signal", "alice", vec![ctrl]);
        let result = state.resolve_sentinel("signal", "alice", "cf:agent:librarian");
        let resolved = result.expect("sentinel should resolve");
        assert_eq!(resolved.label, "Librarian");
        assert!(resolved.command.contains("librarian"));
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn resolve_sentinel_falls_back_to_label() {
        let state = ChoiceState::new();
        state.record("signal", "alice", vec![ctrl_two()]);
        // Use the label as the sentinel — Signal's poll vote emits
        // the title, not the callback_data.
        let result = state.resolve_sentinel("signal", "alice", "Librarian");
        let resolved = result.expect("label fallback should resolve");
        assert_eq!(resolved.label, "Librarian");
    }

    #[test]
    fn ambiguous_reply_does_not_clear_state() {
        let state = ChoiceState::new();
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::agent("Critic", "critic"),
                ChoiceOption::agent("Critique", "critique"),
            ],
        );
        state.record("signal", "alice", vec![ctrl]);
        let result = state.match_reply("signal", "alice", "Cri");
        assert_eq!(result, ChoiceMatchResult::Ambiguous);
        // State preserved so the user can re-try with disambiguation.
        assert_eq!(state.pending_len(), 1);
    }

    #[test]
    fn out_of_range_does_not_clear_state() {
        let state = ChoiceState::new();
        state.record("signal", "alice", vec![ctrl_two()]);
        let result = state.match_reply("signal", "alice", "99");
        assert_eq!(result, ChoiceMatchResult::OutOfRange);
        assert_eq!(state.pending_len(), 1);
    }

    #[test]
    fn evict_expired_drops_only_old_entries() {
        let state = ChoiceState::new();
        state.record("signal", "alice", vec![ctrl_two()]);
        // Inject an already-expired entry directly.
        {
            let mut map = state.inner.lock().unwrap();
            map.insert(
                ("whatsapp".into(), "bob".into()),
                Pending {
                    controls: vec![ctrl_two()],
                    expires_at: Instant::now() - Duration::from_secs(60),
                },
            );
        }
        assert_eq!(state.pending_len(), 2);
        state.evict_expired();
        assert_eq!(state.pending_len(), 1);
    }
}
