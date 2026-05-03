//! Channel-agnostic outbound message envelope.
//!
//! Channels can render this as native media when they support it, or fall back
//! to text links/paths while richer channel senders are being added.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundAttachment {
    pub kind: AttachmentKind,
    pub path: PathBuf,
    pub mime_type: String,
    pub caption: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboundMessage {
    pub text: Option<String>,
    pub attachments: Vec<OutboundAttachment>,
    pub controls: Vec<ChoiceControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceControl {
    pub title: String,
    pub options: Vec<ChoiceOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub label: String,
    pub command: String,
    pub callback_data: Option<String>,
}

impl OutboundMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            attachments: Vec::new(),
            controls: Vec::new(),
        }
    }

    pub fn with_control(mut self, control: ChoiceControl) -> Self {
        if !control.options.is_empty() {
            self.controls.push(control);
        }
        self
    }

    pub fn response_len(&self) -> usize {
        self.render_text_fallback().len()
    }

    pub fn render_text_fallback(&self) -> String {
        let mut rendered = self.text.clone().unwrap_or_default();

        if !self.attachments.is_empty() {
            if !rendered.trim().is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str("Attachments:");
            for attachment in &self.attachments {
                rendered.push_str("\n- ");
                rendered.push_str(&attachment.mime_type);
                rendered.push_str(": ");
                let name = attachment
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("artifact");
                rendered.push_str(name);
                rendered.push_str(" (");
                rendered.push_str(&attachment.size_bytes.to_string());
                rendered.push_str(" bytes)");
                if let Some(caption) = &attachment.caption {
                    if !caption.trim().is_empty() {
                        rendered.push_str(" - ");
                        rendered.push_str(caption.trim());
                    }
                }
            }
        }

        if !self.controls.is_empty() {
            if !rendered.trim().is_empty() {
                rendered.push_str("\n\n");
            }
            for (control_index, control) in self.controls.iter().enumerate() {
                if control_index > 0 {
                    rendered.push('\n');
                }
                if !control.title.trim().is_empty() {
                    rendered.push_str(control.title.trim());
                    rendered.push('\n');
                }
                for option in &control.options {
                    rendered.push_str("- ");
                    rendered.push_str(option.label.trim());
                    rendered.push_str(": `");
                    rendered.push_str(option.command.trim());
                    rendered.push('`');
                    rendered.push('\n');
                }
                if rendered.ends_with('\n') {
                    rendered.pop();
                }
            }
        }

        if rendered.trim().is_empty() {
            "Agent completed without a text response.".to_string()
        } else {
            rendered
        }
    }
}

impl AttachmentKind {
    pub fn from_mime(mime_type: &str) -> Self {
        if mime_type.starts_with("image/") {
            Self::Image
        } else if mime_type.starts_with("audio/") {
            Self::Audio
        } else if mime_type.starts_with("video/") {
            Self::Video
        } else {
            Self::File
        }
    }
}

impl ChoiceControl {
    pub fn new(title: impl Into<String>, options: Vec<ChoiceOption>) -> Self {
        Self {
            title: title.into(),
            options,
        }
    }
}

impl ChoiceOption {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            callback_data: None,
        }
    }

    pub fn agent(label: impl Into<String>, agent_id: impl Into<String>) -> Self {
        let agent_id = agent_id.into();
        Self::new(label, format!("!agent switch {agent_id}"))
            .with_callback_data(format!("cf:agent:{agent_id}"))
    }

    pub fn model(label: impl Into<String>, model_id: impl Into<String>) -> Self {
        let model_id = model_id.into();
        Self::new(label, format!("!model use {model_id}"))
            .with_callback_data(format!("cf:model:{model_id}"))
    }

    pub fn session(
        label: impl Into<String>,
        agent_id: impl Into<String>,
        session: impl Into<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let session = session.into();
        Self::new(label, format!("!switch {agent_id} {session}"))
            .with_callback_data(format!("cf:session:{agent_id}:{session}"))
    }

    pub fn approve(request_id: impl Into<String>) -> Self {
        let request_id = request_id.into();
        Self::new("Approve", format!("!approve {request_id}"))
            .with_callback_data(format!("cf:approve:{request_id}"))
    }

    pub fn deny(request_id: impl Into<String>) -> Self {
        let request_id = request_id.into();
        Self::new("Deny", format!("!deny {request_id}"))
            .with_callback_data(format!("cf:deny:{request_id}"))
    }

    pub fn with_callback_data(mut self, callback_data: impl Into<String>) -> Self {
        self.callback_data = Some(callback_data.into());
        self
    }
}

impl ChoiceControl {
    /// Try to resolve a free-text user reply to one of this control's options.
    ///
    /// The reply is accepted as either a 1-based number ("2", "#3"),
    /// or a fuzzy match against an option label (case-insensitive,
    /// trimmed whitespace, ignoring punctuation, prefix-or-substring).
    /// Ambiguous matches (>1 hit, no exact label/number match) return
    /// `Match::Ambiguous` so the channel can re-prompt instead of
    /// dispatching the wrong action.
    ///
    /// Returns `Match::None` if the reply doesn't match anything —
    /// caller should fall through to normal command/agent dispatch.
    pub fn match_reply(&self, reply: &str) -> Match<'_> {
        let trimmed = reply.trim().trim_start_matches('#').trim();
        if trimmed.is_empty() {
            return Match::None;
        }

        // 1-based number selection: "2", "#3", " 1 ".
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= self.options.len() {
                return Match::One(&self.options[n - 1]);
            }
            // Numeric but out-of-range → tell the caller it was a number
            // attempt that didn't fit. Caller decides whether to re-prompt.
            return Match::OutOfRange;
        }

        // Label match: exact (case/punct/whitespace-insensitive), then
        // prefix, then substring. Each tier short-circuits.
        let normalised_reply = normalize_for_match(trimmed);
        if normalised_reply.is_empty() {
            return Match::None;
        }

        let normalised_options: Vec<String> = self
            .options
            .iter()
            .map(|o| normalize_for_match(&o.label))
            .collect();

        // Exact label match
        let exact: Vec<usize> = normalised_options
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == normalised_reply)
            .map(|(i, _)| i)
            .collect();
        if exact.len() == 1 {
            return Match::One(&self.options[exact[0]]);
        }
        if exact.len() > 1 {
            return Match::Ambiguous;
        }

        // Prefix match
        let prefix: Vec<usize> = normalised_options
            .iter()
            .enumerate()
            .filter(|(_, n)| n.starts_with(&normalised_reply))
            .map(|(i, _)| i)
            .collect();
        if prefix.len() == 1 {
            return Match::One(&self.options[prefix[0]]);
        }
        if prefix.len() > 1 {
            return Match::Ambiguous;
        }

        // Substring match — only if the reply is at least 2 visible
        // characters (counted by Unicode scalar) to avoid silly matches
        // on single letters. `len()` is bytes, which would let a single
        // multi-byte Unicode glyph through unintentionally; `chars().count()`
        // is the right semantic here.
        if normalised_reply.chars().count() >= 2 {
            let substr: Vec<usize> = normalised_options
                .iter()
                .enumerate()
                .filter(|(_, n)| n.contains(&normalised_reply))
                .map(|(i, _)| i)
                .collect();
            if substr.len() == 1 {
                return Match::One(&self.options[substr[0]]);
            }
            if substr.len() > 1 {
                return Match::Ambiguous;
            }
        }

        Match::None
    }
}

/// Result of matching a free-text reply against a `ChoiceControl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match<'a> {
    /// Single unambiguous match — caller should dispatch the option's command.
    One(&'a ChoiceOption),
    /// Reply parsed as a number but no option at that index. Caller may
    /// want to re-prompt with the available range.
    OutOfRange,
    /// Reply matched more than one option. Caller should re-prompt and
    /// ask the user to be more specific.
    Ambiguous,
    /// Reply doesn't look like a selection at all. Caller should treat
    /// the message as freeform input and run normal command/agent dispatch.
    None,
}

fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_does_not_expose_local_artifact_path() {
        let msg = OutboundMessage {
            text: Some("done".to_string()),
            attachments: vec![OutboundAttachment {
                kind: AttachmentKind::Image,
                path: PathBuf::from("/tmp/calciforge-artifacts/run-1/out.png"),
                mime_type: "image/png".to_string(),
                caption: Some("preview".to_string()),
                size_bytes: 42,
            }],
            controls: Vec::new(),
        };

        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("out.png"));
        assert!(rendered.contains("42 bytes"));
        assert!(rendered.contains("preview"));
        assert!(
            !rendered.contains("/tmp/calciforge-artifacts"),
            "chat fallback must not leak local storage paths: {rendered}"
        );
    }

    #[test]
    fn fallback_renders_choice_commands_for_text_only_channels() {
        let msg = OutboundMessage::text("Choose an agent").with_control(ChoiceControl::new(
            "Options",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        ));

        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("Choose an agent"));
        assert!(rendered.contains("Options"));
        assert!(
            rendered.contains("- Librarian: `!agent switch librarian`"),
            "fallback must expose the command users can type: {rendered}"
        );
        assert!(
            rendered.contains("- Critic: `!agent switch critic`"),
            "fallback must include every available choice: {rendered}"
        );
    }

    fn agent_options(labels: &[(&str, &str)]) -> ChoiceControl {
        ChoiceControl::new(
            "Pick one",
            labels
                .iter()
                .map(|(label, id)| ChoiceOption::agent(*label, *id))
                .collect(),
        )
    }

    #[test]
    fn match_reply_resolves_numeric_selection() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Critic", "crit")]);
        assert_eq!(ctrl.match_reply("1"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("2"), Match::One(&ctrl.options[1]));
        assert_eq!(ctrl.match_reply("  #2  "), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn match_reply_reports_out_of_range_for_bad_numbers() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Critic", "crit")]);
        assert_eq!(ctrl.match_reply("0"), Match::OutOfRange);
        assert_eq!(ctrl.match_reply("3"), Match::OutOfRange);
        assert_eq!(ctrl.match_reply("99"), Match::OutOfRange);
    }

    #[test]
    fn match_reply_resolves_exact_label_case_insensitively() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Critic", "crit")]);
        assert_eq!(ctrl.match_reply("Librarian"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("librarian"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("  CRITIC "), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn match_reply_resolves_prefix_when_unambiguous() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Critic", "crit")]);
        assert_eq!(ctrl.match_reply("lib"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("Cri"), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn match_reply_returns_ambiguous_when_prefix_collides() {
        let ctrl = agent_options(&[("Critic", "crit"), ("Critique", "criq")]);
        assert_eq!(ctrl.match_reply("Cri"), Match::Ambiguous);
        // Exact label still wins over prefix collision
        assert_eq!(ctrl.match_reply("Critic"), Match::One(&ctrl.options[0]));
    }

    #[test]
    fn match_reply_returns_none_for_freeform_text() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Critic", "crit")]);
        assert_eq!(ctrl.match_reply(""), Match::None);
        assert_eq!(
            ctrl.match_reply("hey there what's the weather"),
            Match::None,
        );
        assert_eq!(ctrl.match_reply("z"), Match::None);
    }

    #[test]
    fn match_reply_ignores_punctuation_in_label() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::new("Sonnet 4.6", "!model use anthropic/sonnet-4.6"),
                ChoiceOption::new("Opus 4.7 (1M context)", "!model use anthropic/opus-4.7-1m"),
            ],
        );
        assert_eq!(ctrl.match_reply("Sonnet 46"), Match::One(&ctrl.options[0]));
        // Substring match — "Opus" appears uniquely
        assert_eq!(ctrl.match_reply("Opus"), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn render_text_fallback_includes_all_options_and_commands() {
        let msg = OutboundMessage::text("Choose your agent").with_control(ChoiceControl::new(
            "Options",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        ));
        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("Choose your agent"), "{rendered}");
        assert!(rendered.contains("Librarian"), "{rendered}");
        assert!(rendered.contains("Critic"), "{rendered}");
        assert!(rendered.contains("!agent switch librarian"), "{rendered}");
        assert!(rendered.contains("!agent switch critic"), "{rendered}");
    }

    #[test]
    fn match_reply_numbering_matches_control_option_order() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::agent("Alpha", "alpha"),
                ChoiceOption::agent("Beta", "beta"),
                ChoiceOption::agent("Gamma", "gamma"),
            ],
        );
        assert_eq!(ctrl.match_reply("1"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("2"), Match::One(&ctrl.options[1]));
        assert_eq!(ctrl.match_reply("3"), Match::One(&ctrl.options[2]));
        assert_eq!(ctrl.match_reply("0"), Match::OutOfRange);
        assert_eq!(ctrl.match_reply("4"), Match::OutOfRange);
    }

    #[test]
    fn match_reply_label_resolution_handles_special_chars() {
        let ctrl = ChoiceControl::new(
            "Models",
            vec![
                ChoiceOption::new("Claude 4.7 (Opus)", "!model use anthropic/opus-4.7"),
                ChoiceOption::new("GPT-4.1", "!model use openai/gpt-4.1"),
            ],
        );
        assert_eq!(
            ctrl.match_reply("Claude 47 Opus"),
            Match::One(&ctrl.options[0])
        );
        assert_eq!(ctrl.match_reply("GPT41"), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn render_text_fallback_multiple_controls_preserves_all() {
        let msg = OutboundMessage::text("Pick")
            .with_control(ChoiceControl::new(
                "Agents",
                vec![ChoiceOption::agent("Librarian", "librarian")],
            ))
            .with_control(ChoiceControl::new(
                "Sessions",
                vec![ChoiceOption::session("backend", "claude-acpx", "backend")],
            ));
        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("Agents"), "{rendered}");
        assert!(rendered.contains("Sessions"), "{rendered}");
        assert!(rendered.contains("!agent switch librarian"), "{rendered}");
        assert!(
            rendered.contains("!switch claude-acpx backend"),
            "{rendered}"
        );
    }

    #[test]
    fn render_text_fallback_empty_controls_suppressed() {
        let msg = OutboundMessage::text("Hello");
        let rendered = msg.render_text_fallback();
        assert_eq!(rendered, "Hello");
    }

    #[test]
    fn typed_choice_options_keep_text_and_callback_actions_in_sync() {
        let agent = ChoiceOption::agent("Librarian", "librarian");
        assert_eq!(agent.command, "!agent switch librarian");
        assert_eq!(agent.callback_data.as_deref(), Some("cf:agent:librarian"));

        let model = ChoiceOption::model("Fast local", "local/qwen");
        assert_eq!(model.command, "!model use local/qwen");
        assert_eq!(model.callback_data.as_deref(), Some("cf:model:local/qwen"));

        let session = ChoiceOption::session("backend", "claude-acpx", "backend");
        assert_eq!(session.command, "!switch claude-acpx backend");
        assert_eq!(
            session.callback_data.as_deref(),
            Some("cf:session:claude-acpx:backend")
        );

        let approve = ChoiceOption::approve("req-1");
        assert_eq!(approve.command, "!approve req-1");
        assert_eq!(approve.callback_data.as_deref(), Some("cf:approve:req-1"));

        let deny = ChoiceOption::deny("req-1");
        assert_eq!(deny.command, "!deny req-1");
        assert_eq!(deny.callback_data.as_deref(), Some("cf:deny:req-1"));
    }

    #[test]
    fn match_reply_label_that_looks_like_number_prefers_numeric_tier() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::agent("42", "agent-42"),
                ChoiceOption::agent("Regular", "regular"),
            ],
        );
        // "1" should select the first option by numeric index, NOT match
        // label "42" as a substring.
        assert_eq!(ctrl.match_reply("1"), Match::One(&ctrl.options[0]));
        // "42" is parsed as numeric index 42 → out of range, NOT as
        // exact label match. This is the expected tradeoff: numbers
        // always mean index.
        assert_eq!(ctrl.match_reply("42"), Match::OutOfRange);
    }

    #[test]
    fn match_reply_negative_and_zero_numbers() {
        let ctrl = agent_options(&[("Alpha", "a"), ("Beta", "b")]);
        assert_eq!(ctrl.match_reply("0"), Match::OutOfRange);
        // Negative numbers fail to parse as usize → fall through to
        // label matching, which returns None (no label starts with "-").
        assert_eq!(ctrl.match_reply("-1"), Match::None);
        assert_eq!(ctrl.match_reply("-0"), Match::None);
    }

    #[test]
    fn match_reply_huge_number_stays_out_of_range() {
        let ctrl = agent_options(&[("Alpha", "a")]);
        assert_eq!(ctrl.match_reply("999999999"), Match::OutOfRange);
        // usize::MAX as string — should not panic.
        assert_eq!(ctrl.match_reply(&usize::MAX.to_string()), Match::OutOfRange,);
    }

    #[test]
    fn match_reply_single_option_still_requires_selection() {
        let ctrl = ChoiceControl::new("Confirm", vec![ChoiceOption::approve("req-x")]);
        // "1" selects the only option.
        assert_eq!(ctrl.match_reply("1"), Match::One(&ctrl.options[0]));
        // Freeform text doesn't auto-select.
        assert_eq!(ctrl.match_reply("yes"), Match::None);
        assert_eq!(ctrl.match_reply("ok"), Match::None);
    }

    #[test]
    fn match_reply_unicode_labels() {
        let ctrl = ChoiceControl::new(
            "Choose",
            vec![
                ChoiceOption::new("Bibliothécaire", "!agent switch lib"),
                ChoiceOption::new("Crítico", "!agent switch crit"),
            ],
        );
        // Exact match with accented characters.
        assert_eq!(
            ctrl.match_reply("Bibliothécaire"),
            Match::One(&ctrl.options[0]),
        );
        // Prefix match.
        assert_eq!(ctrl.match_reply("Crít"), Match::One(&ctrl.options[1]));
    }

    #[test]
    fn match_reply_emoji_in_label() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::new("📚 Librarian", "!agent switch lib"),
                ChoiceOption::new("🎭 Critic", "!agent switch crit"),
            ],
        );
        // Substring match on text portion.
        assert_eq!(ctrl.match_reply("Librarian"), Match::One(&ctrl.options[0]),);
        assert_eq!(ctrl.match_reply("1"), Match::One(&ctrl.options[0]));
    }

    #[test]
    fn match_reply_whitespace_only_is_none() {
        let ctrl = agent_options(&[("A", "a")]);
        assert_eq!(ctrl.match_reply("   "), Match::None);
        assert_eq!(ctrl.match_reply("\t\n"), Match::None);
        assert_eq!(ctrl.match_reply("  #  "), Match::None);
    }

    #[test]
    fn match_reply_hash_with_number() {
        let ctrl = agent_options(&[("A", "a"), ("B", "b"), ("C", "c")]);
        assert_eq!(ctrl.match_reply("#1"), Match::One(&ctrl.options[0]));
        assert_eq!(ctrl.match_reply("# 3"), Match::One(&ctrl.options[2]));
        assert_eq!(ctrl.match_reply("#0"), Match::OutOfRange);
    }

    #[test]
    fn match_reply_substring_needs_two_chars_minimum() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::new("Alpha Fox", "!a"),
                ChoiceOption::new("Beta Fox", "!b"),
            ],
        );
        // Single-char "F" — both labels contain "fox" but substring
        // matching requires at least 2 chars, so it returns None.
        // (Prefix matching on "f" also fails since neither starts with "f".)
        assert_eq!(ctrl.match_reply("F"), Match::None);
        // Two chars "Fo" — both contain "fo" → substring Ambiguous.
        assert_eq!(ctrl.match_reply("Fo"), Match::Ambiguous);
        // "Fox" is still ambiguous — substring in both.
        assert_eq!(ctrl.match_reply("Fox"), Match::Ambiguous);
        // Unique prefix resolves.
        assert_eq!(ctrl.match_reply("Al"), Match::One(&ctrl.options[0]));
    }

    #[test]
    fn normalize_for_match_strips_punctuation_consistently() {
        assert_eq!(normalize_for_match("Hello, World!"), "hello world");
        assert_eq!(normalize_for_match("v4.7-beta (1M)"), "v47beta 1m");
        assert_eq!(normalize_for_match("  "), "");
        assert_eq!(normalize_for_match("..."), "");
    }

    #[test]
    fn render_text_fallback_no_text_controls_only() {
        let msg = OutboundMessage {
            text: None,
            attachments: vec![],
            controls: vec![ChoiceControl::new(
                "Pick",
                vec![ChoiceOption::agent("Alpha", "alpha")],
            )],
        };
        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("Pick"), "{rendered}");
        assert!(rendered.contains("Alpha"), "{rendered}");
    }

    #[test]
    fn render_text_fallback_no_text_no_controls_shows_placeholder() {
        let msg = OutboundMessage {
            text: None,
            attachments: vec![],
            controls: vec![],
        };
        let rendered = msg.render_text_fallback();
        assert_eq!(rendered, "Agent completed without a text response.");
    }

    #[test]
    fn text_fallback_round_trip_every_option_matchable_by_number() {
        let labels = [
            ("Alpha", "alpha"),
            ("Beta", "beta"),
            ("Gamma", "gamma"),
            ("Delta", "delta"),
            ("Epsilon", "epsilon"),
            ("Zeta", "zeta"),
            ("Eta", "eta"),
            ("Theta", "theta"),
        ];
        let ctrl = agent_options(&labels);
        let msg = OutboundMessage::text("Pick an agent").with_control(ctrl.clone());
        let rendered = msg.render_text_fallback();

        for (i, (label, _id)) in labels.iter().enumerate() {
            assert!(
                rendered.contains(label),
                "rendered fallback must show label '{label}': {rendered}",
            );
            let n = (i + 1).to_string();
            let m = ctrl.match_reply(&n);
            assert_eq!(
                m,
                Match::One(&ctrl.options[i]),
                "option {n} ('{label}') must be matchable by its 1-based index",
            );
        }
        assert_eq!(
            ctrl.match_reply("0"),
            Match::OutOfRange,
            "0 must be out of range",
        );
        assert_eq!(
            ctrl.match_reply(&(labels.len() + 1).to_string()),
            Match::OutOfRange,
            "N+1 must be out of range",
        );
    }

    #[test]
    fn match_reply_negative_number_is_none_not_crash() {
        let ctrl = agent_options(&[("Librarian", "lib")]);
        assert_eq!(ctrl.match_reply("-1"), Match::None);
        assert_eq!(ctrl.match_reply("-999"), Match::None);
    }

    #[test]
    fn match_reply_huge_number_is_out_of_range_not_overflow() {
        let ctrl = agent_options(&[("Librarian", "lib")]);
        assert_eq!(ctrl.match_reply("99999999999999999999"), Match::None);
        assert_eq!(ctrl.match_reply("18446744073709551616"), Match::None);
    }

    #[test]
    fn match_reply_tabs_and_newlines_only_is_none() {
        let ctrl = agent_options(&[("Librarian", "lib")]);
        assert_eq!(ctrl.match_reply("\t\n"), Match::None);
        assert_eq!(ctrl.match_reply("\r\n  \t"), Match::None);
    }

    #[test]
    fn match_reply_unicode_label_matches() {
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::new("Ünïcödé Agent", "!agent switch uni"),
                ChoiceOption::new("日本語エージェント", "!agent switch jp"),
            ],
        );
        assert_eq!(
            ctrl.match_reply("ünïcödé agent"),
            Match::One(&ctrl.options[0])
        );
        assert_eq!(
            ctrl.match_reply("日本語エージェント"),
            Match::One(&ctrl.options[1])
        );
    }

    #[test]
    fn match_reply_single_char_substring_does_not_match() {
        let ctrl = agent_options(&[("Librarian", "lib"), ("Linguist", "ling")]);
        assert_eq!(ctrl.match_reply("L"), Match::Ambiguous);
        assert_eq!(ctrl.match_reply("x"), Match::None);
    }

    #[test]
    fn match_reply_very_long_input_is_none() {
        let ctrl = agent_options(&[("Librarian", "lib")]);
        let long = "a".repeat(10_000);
        assert_eq!(ctrl.match_reply(&long), Match::None);
    }

    #[test]
    fn text_fallback_round_trip_every_option_matchable_by_label() {
        let options = vec![
            ChoiceOption::agent("Librarian", "librarian"),
            ChoiceOption::model("Claude Sonnet", "anthropic/sonnet-4.6"),
            ChoiceOption::session("backend", "codex", "backend"),
            ChoiceOption::approve("req-42"),
            ChoiceOption::deny("req-42"),
        ];
        let ctrl = ChoiceControl::new("Actions", options);
        let msg = OutboundMessage::text("What next?").with_control(ctrl.clone());
        let _rendered = msg.render_text_fallback();

        let label_queries = ["Librarian", "Claude Sonnet", "backend", "Approve", "Deny"];
        for (i, query) in label_queries.iter().enumerate() {
            let m = ctrl.match_reply(query);
            assert_eq!(
                m,
                Match::One(&ctrl.options[i]),
                "label '{query}' must match option {i}",
            );
        }
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_agent_options() -> impl Strategy<Value = ChoiceControl> {
            prop::collection::vec("[A-Za-z][A-Za-z0-9 ]{0,20}", 1..=8).prop_map(|labels| {
                let options: Vec<ChoiceOption> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, l)| ChoiceOption::agent(l.trim(), &format!("agent-{i}")))
                    .collect();
                ChoiceControl::new("Pick", options)
            })
        }

        proptest! {
            #[test]
            fn match_reply_never_panics(input in ".*") {
                let ctrl = ChoiceControl::new("Pick", vec![
                    ChoiceOption::agent("Alpha", "alpha"),
                    ChoiceOption::agent("Beta", "beta"),
                ]);
                let _ = ctrl.match_reply(&input);
            }

            #[test]
            fn valid_index_always_matches(
                ctrl in arb_agent_options(),
                idx in 1usize..=8,
            ) {
                if idx <= ctrl.options.len() {
                    let r = ctrl.match_reply(&idx.to_string());
                    prop_assert!(matches!(r, Match::One(_)),
                        "index {idx} within range should match, got {:?}", r);
                }
            }

            #[test]
            fn out_of_range_index_never_matches(
                ctrl in arb_agent_options(),
                idx in 9usize..1000,
            ) {
                let r = ctrl.match_reply(&idx.to_string());
                prop_assert!(matches!(r, Match::OutOfRange),
                    "index {idx} above max should be OutOfRange, got {:?}", r);
            }

            #[test]
            fn exact_label_always_resolves_to_some_match(
                ctrl in arb_agent_options(),
            ) {
                for option in &ctrl.options {
                    let r = ctrl.match_reply(&option.label);
                    prop_assert!(
                        matches!(r, Match::One(_) | Match::Ambiguous),
                        "exact label {:?} should resolve to One or Ambiguous, got {:?}",
                        option.label, r
                    );
                }
            }

            #[test]
            fn match_result_is_deterministic(
                ctrl in arb_agent_options(),
                input in ".*",
            ) {
                let r1 = ctrl.match_reply(&input);
                let r2 = ctrl.match_reply(&input);
                prop_assert_eq!(r1, r2);
            }
        }
    }
}
