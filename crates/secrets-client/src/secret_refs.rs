//! Shared secret-reference helpers for MCP and CLI discovery surfaces.
//!
//! These helpers never resolve values. They only validate secret names and
//! build the placeholder token consumed by the security proxy.

/// Return true when a name can be embedded in `{{secret:NAME}}`.
pub fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// Build the canonical placeholder token for a valid secret name.
pub fn secret_reference_token(name: &str) -> Option<String> {
    if is_valid_secret_name(name) {
        Some(format!("{{{{secret:{name}}}}}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_secret_name_shape() {
        assert!(is_valid_secret_name("BRAVE_API_KEY"));
        assert!(is_valid_secret_name("anthropic-key"));
        assert!(!is_valid_secret_name(""));
        assert!(!is_valid_secret_name("bad/name"));
        assert!(!is_valid_secret_name("bad name"));
    }

    #[test]
    fn builds_canonical_reference_token() {
        assert_eq!(
            secret_reference_token("BRAVE_API_KEY").as_deref(),
            Some("{{secret:BRAVE_API_KEY}}")
        );
        assert!(secret_reference_token("bad/name").is_none());
    }
}
