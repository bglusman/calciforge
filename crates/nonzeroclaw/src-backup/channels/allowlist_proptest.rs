/// Property-based allowlist tests for all channel types.
///
/// These tests verify the core invariant: the allowlist matching function
/// must be an EXACT match — no substring matching, no prefix matching,
/// and no normalization mismatch should allow a non-allowed identity through.
#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    // super = allowlist_proptest, super::super = channels
    use super::super::whatsapp::WhatsAppChannel;
    use super::super::discord::DiscordChannel;
    use super::super::imessage::IMessageChannel;
    use super::super::telegram::TelegramChannel;
    use super::super::signal::SignalChannel;

    // ── WhatsApp: E.164 phone number allowlist ──────────────────

    proptest! {
        /// If `a` is on the allowlist (normalized to +prefix), then only exact
        /// matches should be allowed — a different number `b` must be denied.
        #[test]
        fn whatsapp_exact_match_prop(a in "\\+?\\d{7,15}", b in "\\+?\\d{7,15}") {
            let norm_a = format!("+{}", a.trim_start_matches('+'));
            let norm_b = format!("+{}", b.trim_start_matches('+'));
            let allowed = vec![norm_a.clone()];
            let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), allowed);
            prop_assert_eq!(ch.is_number_allowed(&norm_b), norm_a == norm_b);
        }

        /// A number that is a proper prefix of the allowed number must NOT match.
        /// This directly tests for the substring/prefix-matching bug class.
        #[test]
        fn whatsapp_prefix_must_not_match(base in "\\d{7,12}") {
            let allowed_number = format!("+{base}999");
            let shorter_number = format!("+{base}");
            let ch = WhatsAppChannel::new(
                "t".into(), "id".into(), "v".into(),
                vec![allowed_number.clone()],
            );
            prop_assert!(!ch.is_number_allowed(&shorter_number),
                "shorter number {} must not match allowed {}", shorter_number, allowed_number);
            prop_assert!(ch.is_number_allowed(&allowed_number));
        }

        /// Appending digits to an allowed number must NOT match.
        #[test]
        fn whatsapp_suffix_must_not_match(base in "\\d{7,12}") {
            let allowed_number = format!("+{base}");
            let longer_number = format!("+{base}999");
            let ch = WhatsAppChannel::new(
                "t".into(), "id".into(), "v".into(),
                vec![allowed_number.clone()],
            );
            prop_assert!(!ch.is_number_allowed(&longer_number),
                "longer number {} must not match allowed {}", longer_number, allowed_number);
        }

        /// Empty allowlist must deny all numbers.
        #[test]
        fn whatsapp_empty_allowlist_denies_all(phone in "\\+\\d{7,15}") {
            let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec![]);
            prop_assert!(!ch.is_number_allowed(&phone));
        }

        /// Wildcard allowlist must allow all numbers.
        #[test]
        fn whatsapp_wildcard_allows_all(phone in "\\+\\d{7,15}") {
            let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec!["*".into()]);
            prop_assert!(ch.is_number_allowed(&phone));
        }
    }

    // ── Discord: user ID allowlist ──────────────────────────────

    proptest! {
        #[test]
        fn discord_exact_match_prop(a in "[0-9]{6,20}", b in "[0-9]{6,20}") {
            prop_assume!(a != b);
            let allowed = vec![a.clone()];
            let ch = DiscordChannel::new("token".into(), None, allowed, false, false);
            prop_assert!(ch.is_user_allowed(&a));
            prop_assert!(!ch.is_user_allowed(&b));
        }

        #[test]
        fn discord_empty_allowlist_denies_all(uid in "[0-9]{6,20}") {
            let ch = DiscordChannel::new("token".into(), None, vec![], false, false);
            prop_assert!(!ch.is_user_allowed(&uid));
        }

        #[test]
        fn discord_wildcard_allows_all(uid in "[0-9]{6,20}") {
            let ch = DiscordChannel::new("token".into(), None, vec!["*".into()], false, false);
            prop_assert!(ch.is_user_allowed(&uid));
        }

        /// Prefix of allowed user ID must not match.
        #[test]
        fn discord_prefix_must_not_match(base in "[0-9]{6,15}") {
            let allowed_id = format!("{base}999");
            let shorter_id = base.clone();
            let ch = DiscordChannel::new("token".into(), None, vec![allowed_id.clone()], false, false);
            prop_assert!(!ch.is_user_allowed(&shorter_id),
                "shorter id {} must not match allowed {}", shorter_id, allowed_id);
        }
    }

    // ── iMessage: contact allowlist (case-insensitive) ──────────

    proptest! {
        #[test]
        fn imessage_exact_match_prop(
            a in "[a-z0-9._%+-]+@[a-z0-9.-]+\\.[a-z]{2,4}",
            b in "[a-z0-9._%+-]+@[a-z0-9.-]+\\.[a-z]{2,4}",
        ) {
            prop_assume!(a.to_lowercase() != b.to_lowercase());
            let ch = IMessageChannel::new(vec![a.clone()]);
            prop_assert!(ch.is_contact_allowed(&a));
            prop_assert!(!ch.is_contact_allowed(&b));
        }

        /// iMessage uses case-insensitive matching — verify uppercased still matches.
        #[test]
        fn imessage_case_insensitive(contact in "[a-z]{3,10}@[a-z]{3,6}\\.com") {
            let ch = IMessageChannel::new(vec![contact.clone()]);
            prop_assert!(ch.is_contact_allowed(&contact.to_uppercase()));
        }

        #[test]
        fn imessage_empty_allowlist_denies_all(contact in "[a-z]{3,10}@example\\.com") {
            let ch = IMessageChannel::new(vec![]);
            prop_assert!(!ch.is_contact_allowed(&contact));
        }

        #[test]
        fn imessage_wildcard_allows_all(contact in "[a-z]{3,10}@example\\.com") {
            let ch = IMessageChannel::new(vec!["*".into()]);
            prop_assert!(ch.is_contact_allowed(&contact));
        }
    }

    // ── Telegram: username/ID allowlist (normalized: strip @, lowercase) ─

    proptest! {
        /// Telegram normalizes by stripping leading `@` and lowercasing.
        #[test]
        fn telegram_exact_match_prop(a in "[a-zA-Z][a-zA-Z0-9_]{4,30}", b in "[a-zA-Z][a-zA-Z0-9_]{4,30}") {
            let norm_a = a.trim_start_matches('@').to_lowercase();
            let norm_b = b.trim_start_matches('@').to_lowercase();
            prop_assume!(norm_a != norm_b);
            let ch = TelegramChannel::new("dummy_token".into(), vec![a.clone()], false);
            prop_assert!(ch.is_user_allowed(&a));
            // b should only pass if it normalizes to the same as a
            prop_assert_eq!(ch.is_user_allowed(&b), norm_a == norm_b);
        }

        /// Telegram should normalize @-prefix: `@user` and `user` are the same.
        #[test]
        fn telegram_at_prefix_normalization(user in "[a-zA-Z][a-zA-Z0-9_]{4,20}") {
            let with_at = format!("@{user}");
            let ch = TelegramChannel::new("dummy_token".into(), vec![with_at.clone()], false);
            // Both forms should match
            prop_assert!(ch.is_user_allowed(&user));
            prop_assert!(ch.is_user_allowed(&with_at));
        }

        #[test]
        fn telegram_empty_allowlist_denies_all(user in "[a-zA-Z][a-zA-Z0-9_]{4,20}") {
            // Note: empty allowlist in Telegram triggers pairing mode, but
            // is_user_allowed still checks against the (empty) list.
            let ch = TelegramChannel::new("dummy_token".into(), vec![], false);
            prop_assert!(!ch.is_user_allowed(&user));
        }

        #[test]
        fn telegram_wildcard_allows_all(user in "[a-zA-Z][a-zA-Z0-9_]{4,20}") {
            let ch = TelegramChannel::new("dummy_token".into(), vec!["*".into()], false);
            prop_assert!(ch.is_user_allowed(&user));
        }
    }

    // ── Signal: E.164 phone number allowlist ────────────────────

    proptest! {
        #[test]
        fn signal_exact_match_prop(a in "\\+\\d{7,15}", b in "\\+\\d{7,15}") {
            prop_assume!(a != b);
            let ch = SignalChannel::new(
                "http://localhost:8080".into(),
                "+10000000000".into(),
                None,
                vec![a.clone()],
                false,
                false,
            );
            prop_assert!(ch.is_sender_allowed(&a));
            prop_assert!(!ch.is_sender_allowed(&b));
        }

        #[test]
        fn signal_prefix_must_not_match(base in "\\d{7,12}") {
            let allowed = format!("+{base}999");
            let shorter = format!("+{base}");
            let ch = SignalChannel::new(
                "http://localhost:8080".into(),
                "+10000000000".into(),
                None,
                vec![allowed.clone()],
                false,
                false,
            );
            prop_assert!(!ch.is_sender_allowed(&shorter),
                "shorter {} must not match allowed {}", shorter, allowed);
        }

        #[test]
        fn signal_empty_allowlist_denies_all(phone in "\\+\\d{7,15}") {
            let ch = SignalChannel::new(
                "http://localhost:8080".into(),
                "+10000000000".into(),
                None,
                vec![],
                false,
                false,
            );
            prop_assert!(!ch.is_sender_allowed(&phone));
        }

        #[test]
        fn signal_wildcard_allows_all(phone in "\\+\\d{7,15}") {
            let ch = SignalChannel::new(
                "http://localhost:8080".into(),
                "+10000000000".into(),
                None,
                vec!["*".into()],
                false,
                false,
            );
            prop_assert!(ch.is_sender_allowed(&phone));
        }
    }
}
