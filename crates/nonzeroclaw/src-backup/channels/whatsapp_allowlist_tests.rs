/// Deterministic unit tests for WhatsApp allowlist matching.
/// Complement the property-based tests in `allowlist_proptest.rs`.
#[cfg(test)]
mod tests {
    use super::super::whatsapp::WhatsAppChannel;

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec![]);
        assert!(!ch.is_number_allowed("+15551234567"));
    }

    #[test]
    fn exact_allowlist_allows_only_exact() {
        let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec!["+15550000001".into()]);
        assert!(ch.is_number_allowed("+15550000001"));
        assert!(!ch.is_number_allowed("+15550000002"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec!["*".into()]);
        assert!(ch.is_number_allowed("+1"));
        assert!(ch.is_number_allowed("+999999999"));
    }

    #[test]
    fn normalization_should_match_missing_plus() {
        let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), vec!["+15550000001".into()]);
        // The parsing code normalizes incoming numbers by prepending '+', so ensure same matching logic
        let normalized = if "15550000001".starts_with('+') { "15550000001".to_string() } else { format!("+{}", "15550000001") };
        assert!(ch.is_number_allowed(&normalized));
    }

    #[test]
    fn random_property_like_matching_examples() {
        let allowed = vec!["+1000".into(), "+2000".into()];
        let ch = WhatsAppChannel::new("t".into(), "id".into(), "v".into(), allowed);
        // a few deterministic checks that would have caught a substring bug
        assert!(!ch.is_number_allowed("+10000"), "+10000 should not match +1000 by substring");
        assert!(ch.is_number_allowed("+2000"));
    }
}
