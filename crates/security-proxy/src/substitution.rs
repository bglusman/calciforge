//! `{{secret:NAME}}` reference substitution.
//!
//! This module is the core of task #29 per
//! `docs/rfcs/agent-secret-gateway.md` §3: agents never see raw secret
//! values, but they can freely write `{{secret:NAME}}` tokens into
//! URLs, headers, and JSON bodies. The gateway substitutes at forward
//! time so the real value only ever exists in memory during the
//! outbound request.
//!
//! ## API shape
//!
//! Two-phase to separate parsing from async I/O:
//!
//! 1. [`find_refs`] — sync parse pass that extracts the unique set of
//!    reference names from the input and validates syntax. Fails on
//!    malformed or nested refs.
//! 2. [`substitute`] — sync render pass that consumes a `(name →
//!    value)` map from the caller and produces the final string. The
//!    caller is responsible for resolving names (env, fnox,
//!    vaultwarden) between the two calls; that resolution can be
//!    parallel, cached, policy-checked, etc., independent of this
//!    module.
//!
//! ## Contract
//!
//! - Token syntax: `{{secret:NAME}}` where NAME matches `[A-Za-z0-9_-]+`.
//! - On unresolvable ref: `substitute` returns `Err(Unresolvable)` —
//!   the caller MUST fail the outbound request rather than forward the
//!   literal (which would leak the name to the upstream).
//! - Nested refs (`{{secret:{{secret:X}}}}`) are rejected at parse time.
//! - Resolved values are NOT re-scanned for refs. Substitution is a
//!   single pass.
//!
//! Placeholder-injection mode (roadmap #151) uses the same policy-gated
//! resolver path, but starts from opaque placeholder values such as
//! `cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef` instead of explicit
//! `{{secret:NAME}}` refs. The scanner in this module only recognizes
//! placeholder tokens; callers must still resolve token -> secret name
//! through an authoritative per-agent map before loading any secret.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

pub const PLACEHOLDER_PREFIX: &str = "cfg_";
pub const PLACEHOLDER_RANDOM_HEX_LEN: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlaceholderMapError {
    #[error("placeholder agent id must not be empty")]
    EmptyAgentId,

    #[error("invalid placeholder token: {0}")]
    InvalidToken(String),

    #[error("invalid placeholder secret name: {0}")]
    InvalidSecretName(String),

    #[error("placeholder token {token:?} is already registered for another secret")]
    ConflictingToken { token: String },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlaceholderResolutionError {
    #[error("placeholder token {0:?} is not registered for the current agent")]
    UnknownToken(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubstitutionError {
    #[error("secret reference {{{{secret:{0}}}}} could not be resolved")]
    Unresolvable(String),

    #[error("secret placeholder {0:?} could not be resolved")]
    UnresolvablePlaceholder(String),

    #[error("nested secret references are not permitted")]
    Nested,

    #[error("malformed secret reference: {0}")]
    Malformed(String),
}

/// Per-agent placeholder token registry.
///
/// This map intentionally resolves only `agent_id + full opaque token` to a
/// secret name. It does not trust the token's embedded name hint, and it does
/// not load secret values. Callers must pass the returned secret name through
/// the normal identity ACL and destination allowlist before resolving a value.
#[derive(Debug, Clone, Default)]
pub struct PlaceholderMap {
    by_agent: HashMap<String, HashMap<String, String>>,
}

impl PlaceholderMap {
    pub fn insert(
        &mut self,
        agent_id: impl Into<String>,
        token: impl Into<String>,
        secret_name: impl Into<String>,
    ) -> Result<(), PlaceholderMapError> {
        let agent_id = agent_id.into();
        if agent_id.trim().is_empty() {
            return Err(PlaceholderMapError::EmptyAgentId);
        }

        let token = token.into();
        if placeholder_name_hint(&token).is_none() {
            return Err(PlaceholderMapError::InvalidToken(token));
        }

        let secret_name = secret_name.into();
        if let Err(error) = validate_name(&secret_name) {
            return Err(PlaceholderMapError::InvalidSecretName(error.to_string()));
        }

        let agent_tokens = self.by_agent.entry(agent_id).or_default();
        if let Some(existing) = agent_tokens.get(&token) {
            if existing != &secret_name {
                return Err(PlaceholderMapError::ConflictingToken { token });
            }
            return Ok(());
        }
        agent_tokens.insert(token, secret_name);
        Ok(())
    }

    pub fn resolve<'a>(
        &'a self,
        identity: &secrets_client::SecretAccessIdentity,
        token: &str,
    ) -> Option<&'a str> {
        let agent_id = identity.agent_id.as_deref()?;
        self.by_agent.get(agent_id)?.get(token).map(String::as_str)
    }

    pub fn remove(&mut self, agent_id: &str, token: &str) -> bool {
        let Some(agent_tokens) = self.by_agent.get_mut(agent_id) else {
            return false;
        };

        let removed = agent_tokens.remove(token).is_some();
        if agent_tokens.is_empty() {
            self.by_agent.remove(agent_id);
        }
        removed
    }

    pub fn resolve_tokens(
        &self,
        identity: &secrets_client::SecretAccessIdentity,
        tokens: &HashSet<String>,
    ) -> Result<HashMap<String, String>, PlaceholderResolutionError> {
        let mut resolved = HashMap::new();
        for token in tokens {
            let secret_name = self
                .resolve(identity, token)
                .ok_or_else(|| PlaceholderResolutionError::UnknownToken(token.clone()))?;
            resolved.insert(token.clone(), secret_name.to_string());
        }
        Ok(resolved)
    }
}

/// Generate an opaque placeholder token for a validated secret name.
///
/// The embedded secret name is only a diagnostic hint. Runtime substitution
/// must still resolve the full token through [`PlaceholderMap`] before loading
/// any secret value.
pub fn generate_placeholder_token(secret_name: &str) -> Result<String, PlaceholderMapError> {
    if let Err(error) = validate_name(secret_name) {
        return Err(PlaceholderMapError::InvalidSecretName(error.to_string()));
    }

    Ok(format!(
        "{}{}_{}",
        PLACEHOLDER_PREFIX,
        secret_name,
        uuid::Uuid::new_v4().as_simple()
    ))
}

/// Parse `input` and return the set of unique reference names it
/// contains. Caller typically resolves these (possibly in parallel)
/// before calling [`substitute`].
///
/// Fails on malformed or nested references. Safe to call on huge
/// strings — it's a single O(n) scan.
pub fn find_refs(input: &str) -> Result<HashSet<String>, SubstitutionError> {
    let mut names = HashSet::new();
    let mut rest = input;

    while let Some(start) = rest.find("{{secret:") {
        // Advance past any non-ref prefix.
        let after_prefix = &rest[start + 9..];
        // Nested-ref detection: if we see `{{` before `}}`, reject.
        let close = match (after_prefix.find("}}"), after_prefix.find("{{")) {
            (Some(c), Some(n)) if n < c => return Err(SubstitutionError::Nested),
            (Some(c), _) => c,
            (None, _) => {
                return Err(SubstitutionError::Malformed(
                    "unterminated secret reference".to_string(),
                ));
            }
        };
        let name = &after_prefix[..close];
        validate_name(name)?;
        names.insert(name.to_string());
        rest = &after_prefix[close + 2..];
    }

    Ok(names)
}

/// Find unique placeholder-injection tokens in `input`.
///
/// Valid placeholders have shape `cfg_<NAME>_<32-hex>`, where `<NAME>`
/// uses the same syntax as explicit `{{secret:NAME}}` references. The
/// returned token is the full opaque placeholder string, not the embedded
/// secret-name hint; the caller must use an authoritative per-agent
/// placeholder map to decide which secret, if any, the token may resolve to.
///
/// Malformed `cfg_`-prefixed strings are ignored instead of failing the
/// request. That keeps ordinary configuration identifiers from becoming a
/// proxy-level denial of service while still allowing valid generated
/// placeholders to be found cheaply.
pub fn find_placeholder_tokens(input: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    for (start, end) in placeholder_token_spans(input) {
        tokens.insert(input[start..end].to_string());
    }
    tokens
}

fn placeholder_token_spans(input: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let bytes = input.as_bytes();
    let mut search_start = 0;

    while let Some(relative_start) = input[search_start..].find(PLACEHOLDER_PREFIX) {
        let start = search_start + relative_start;
        if start > 0 && is_placeholder_token_byte(bytes[start - 1]) {
            search_start = start + PLACEHOLDER_PREFIX.len();
            continue;
        }

        let mut end = start;
        while end < bytes.len() && is_placeholder_token_byte(bytes[end]) {
            end += 1;
        }

        let token = &input[start..end];
        if placeholder_name_hint(token).is_some() {
            spans.push((start, end));
        }
        search_start = end.max(start + PLACEHOLDER_PREFIX.len());
    }

    spans
}

/// Return the embedded secret-name hint for a syntactically valid placeholder.
///
/// This is only a hint for diagnostics and generation tests. Runtime
/// substitution must use the per-agent placeholder map rather than trusting
/// this embedded name.
pub fn placeholder_name_hint(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(PLACEHOLDER_PREFIX)?;
    let (name, suffix) = rest.rsplit_once('_')?;
    if suffix.len() != PLACEHOLDER_RANDOM_HEX_LEN
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        || validate_name(name).is_err()
    {
        return None;
    }
    Some(name)
}

/// Render `input`, replacing every `{{secret:NAME}}` with the value
/// from `resolved`. Returns `Cow::Borrowed(input)` when no refs are
/// present (zero allocation).
///
/// Errors if any ref is not in the map (caller's responsibility to
/// have resolved everything `find_refs` returned).
pub fn substitute<'a, Map: RefMap>(
    input: &'a str,
    resolved: &Map,
) -> Result<Cow<'a, str>, SubstitutionError> {
    if !input.contains("{{secret:") {
        return Ok(Cow::Borrowed(input));
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{secret:") {
        out.push_str(&rest[..start]);
        let after_prefix = &rest[start + 9..];
        let close = match (after_prefix.find("}}"), after_prefix.find("{{")) {
            (Some(c), Some(n)) if n < c => return Err(SubstitutionError::Nested),
            (Some(c), _) => c,
            (None, _) => {
                return Err(SubstitutionError::Malformed(
                    "unterminated secret reference".to_string(),
                ));
            }
        };
        let name = &after_prefix[..close];
        validate_name(name)?;
        let value = resolved
            .get(name)
            .ok_or_else(|| SubstitutionError::Unresolvable(name.to_string()))?;
        out.push_str(value);
        rest = &after_prefix[close + 2..];
    }
    out.push_str(rest);

    Ok(Cow::Owned(out))
}

/// Render `input`, replacing every valid placeholder token with the value
/// from `resolved`.
///
/// The `resolved` map is keyed by the full opaque placeholder token, not by
/// the embedded name hint. Callers are responsible for resolving
/// placeholder token -> secret name through the per-agent map, then applying
/// the normal identity and destination policy gates before passing values
/// here. Malformed `cfg_` candidates are preserved unchanged.
pub fn substitute_placeholders<'a, Map: RefMap>(
    input: &'a str,
    resolved: &Map,
) -> Result<Cow<'a, str>, SubstitutionError> {
    let spans = placeholder_token_spans(input);
    if spans.is_empty() {
        return Ok(Cow::Borrowed(input));
    }

    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for (start, end) in spans {
        out.push_str(&input[cursor..start]);
        let token = &input[start..end];
        let value = resolved
            .get(token)
            .ok_or_else(|| SubstitutionError::UnresolvablePlaceholder(token.to_string()))?;
        out.push_str(value);
        cursor = end;
    }
    out.push_str(&input[cursor..]);

    Ok(Cow::Owned(out))
}

fn validate_name(name: &str) -> Result<(), SubstitutionError> {
    if name.is_empty() {
        return Err(SubstitutionError::Malformed(
            "empty secret name ({{secret:}})".to_string(),
        ));
    }
    if !name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(SubstitutionError::Malformed(format!(
            "secret name {name:?} contains invalid characters (allowed: A-Z a-z 0-9 _ -)"
        )));
    }
    Ok(())
}

fn is_placeholder_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Abstraction over the map `substitute` looks up names in. Implemented
/// for `HashMap<String, String>` and `&[(String, String)]` so tests and
/// call sites can hand in whatever's convenient.
pub trait RefMap {
    fn get(&self, name: &str) -> Option<&str>;
}

impl RefMap for std::collections::HashMap<String, String> {
    fn get(&self, name: &str) -> Option<&str> {
        self.get(name).map(String::as_str)
    }
}

impl RefMap for &std::collections::HashMap<String, String> {
    fn get(&self, name: &str) -> Option<&str> {
        (**self).get(name).map(String::as_str)
    }
}

impl RefMap for &[(String, String)] {
    fn get(&self, name: &str) -> Option<&str> {
        self.iter()
            .find_map(|(k, v)| (k == name).then_some(v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ── find_refs ────────────────────────────────────────────────

    /// Given input with no refs,
    /// when find_refs is called,
    /// then the returned set is empty.
    #[test]
    fn find_refs_returns_empty_for_input_without_refs() {
        let refs = find_refs("plain text https://example.com?a=b").unwrap();
        assert!(refs.is_empty());
    }

    /// Given input with two distinct refs used three times,
    /// when find_refs is called,
    /// then the returned set contains exactly two unique names.
    #[test]
    fn find_refs_deduplicates() {
        let refs = find_refs("{{secret:A}} x {{secret:B}} y {{secret:A}}").unwrap();
        assert_eq!(refs.len(), 2);
        assert!(refs.contains("A"));
        assert!(refs.contains("B"));
    }

    /// Given input with a nested ref,
    /// when find_refs is called,
    /// then Nested is returned and no names are collected.
    #[test]
    fn find_refs_rejects_nested() {
        assert_eq!(
            find_refs("{{secret:{{secret:INNER}}}}"),
            Err(SubstitutionError::Nested)
        );
    }

    /// Given input with an unterminated ref,
    /// when find_refs is called,
    /// then Malformed is returned (important — do not treat
    /// everything-to-EOF as a name, which would exfiltrate input
    /// shape to the resolver).
    #[test]
    fn find_refs_rejects_unterminated() {
        let err = find_refs("{{secret:NO_CLOSE").unwrap_err();
        assert!(matches!(err, SubstitutionError::Malformed(_)));
    }

    /// Given a ref name with invalid characters,
    /// when find_refs is called,
    /// then Malformed is returned.
    #[test]
    fn find_refs_rejects_invalid_name_chars() {
        let cases = ["{{secret:FOO BAR}}", "{{secret:a/b}}", "{{secret:a.b}}"];
        for input in cases {
            let err = find_refs(input).unwrap_err();
            assert!(
                matches!(err, SubstitutionError::Malformed(_)),
                "expected Malformed for {input:?}, got {err:?}"
            );
        }
    }

    // ── placeholder tokens ──────────────────────────────────────

    /// Given an input with repeated valid placeholder-injection tokens,
    /// when find_placeholder_tokens is called,
    /// then it returns the unique opaque token strings.
    #[test]
    fn find_placeholder_tokens_deduplicates_valid_tokens() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let other = "cfg_DATABASE-URL_ffffffffffffffffffffffffffffffff";
        let found = find_placeholder_tokens(&format!("Bearer {token}; again={token}; db={other}"));

        assert_eq!(found.len(), 2);
        assert!(found.contains(token));
        assert!(found.contains(other));
    }

    /// Given a valid placeholder token,
    /// when placeholder_name_hint is called,
    /// then it returns the embedded diagnostic hint only.
    #[test]
    fn placeholder_name_hint_accepts_valid_shape() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        assert_eq!(placeholder_name_hint(token), Some("OPENAI_KEY"));
    }

    /// Given a valid secret name,
    /// when generate_placeholder_token is called,
    /// then it returns a syntactically valid opaque token with the secret
    /// name only as a diagnostic hint.
    #[test]
    fn generate_placeholder_token_returns_valid_opaque_token() {
        let token = generate_placeholder_token("OPENAI_API_KEY").unwrap();

        assert_eq!(placeholder_name_hint(&token), Some("OPENAI_API_KEY"));
        assert!(find_placeholder_tokens(&token).contains(&token));
        assert_eq!(
            token.rsplit_once('_').map(|(_, suffix)| suffix.len()),
            Some(PLACEHOLDER_RANDOM_HEX_LEN)
        );
    }

    /// Given an invalid secret name,
    /// when generate_placeholder_token is called,
    /// then it fails before creating a token that cannot be registered.
    #[test]
    fn generate_placeholder_token_rejects_invalid_secret_name() {
        assert!(matches!(
            generate_placeholder_token("OPENAI.API.KEY"),
            Err(PlaceholderMapError::InvalidSecretName(_))
        ));
    }

    /// Given cfg-prefixed strings that do not match the generated
    /// placeholder shape,
    /// when find_placeholder_tokens is called,
    /// then they are ignored so ordinary config identifiers do not
    /// fail outbound requests.
    #[test]
    fn find_placeholder_tokens_ignores_malformed_candidates() {
        let found = find_placeholder_tokens(
            "cfg_ cfg_short_deadbeef cfg_BAD_SUFFIX_nothex cfg_BAD.NAME_0123456789abcdef0123456789abcdef",
        );
        assert!(found.is_empty());
    }

    /// Given a placeholder-looking token embedded inside a larger
    /// identifier,
    /// when find_placeholder_tokens is called,
    /// then it is ignored rather than partially matching.
    #[test]
    fn find_placeholder_tokens_requires_token_boundary() {
        let found =
            find_placeholder_tokens("prefixcfg_OPENAI_KEY_0123456789abcdef0123456789abcdef");
        assert!(found.is_empty());
    }

    /// Given a placeholder registered for one agent,
    /// when that same agent resolves the full token,
    /// then the authoritative secret name is returned.
    #[test]
    fn placeholder_map_resolves_for_matching_agent() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let identity = secrets_client::SecretAccessIdentity {
            agent_id: Some("agent-a".to_string()),
            user_id: Some("user-1".to_string()),
            channel: Some("signal".to_string()),
        };
        let mut map = PlaceholderMap::default();

        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();

        assert_eq!(map.resolve(&identity, token), Some("OPENAI_API_KEY"));
    }

    /// Given the same opaque placeholder token is registered under another
    /// agent,
    /// when a different agent tries to resolve it,
    /// then no secret name is returned.
    #[test]
    fn placeholder_map_is_scoped_by_agent_id() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let identity = secrets_client::SecretAccessIdentity {
            agent_id: Some("agent-b".to_string()),
            user_id: None,
            channel: None,
        };
        let mut map = PlaceholderMap::default();

        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();

        assert_eq!(map.resolve(&identity, token), None);
    }

    /// Given an identity without an agent id,
    /// when it tries to resolve a placeholder,
    /// then resolution fails closed.
    #[test]
    fn placeholder_map_requires_agent_identity() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let identity = secrets_client::SecretAccessIdentity::default();
        let mut map = PlaceholderMap::default();

        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();

        assert_eq!(map.resolve(&identity, token), None);
    }

    /// Given invalid registration inputs,
    /// when inserting into the placeholder map,
    /// then invalid agent, token, and secret names are rejected early.
    #[test]
    fn placeholder_map_rejects_invalid_registration() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let mut map = PlaceholderMap::default();

        assert_eq!(
            map.insert(" ", token, "OPENAI_API_KEY"),
            Err(PlaceholderMapError::EmptyAgentId)
        );
        assert_eq!(
            map.insert("agent-a", "cfg_short_deadbeef", "OPENAI_API_KEY"),
            Err(PlaceholderMapError::InvalidToken(
                "cfg_short_deadbeef".to_string()
            ))
        );
        assert!(matches!(
            map.insert("agent-a", token, "OPENAI.API.KEY"),
            Err(PlaceholderMapError::InvalidSecretName(_))
        ));
    }

    /// Given a placeholder token is already registered for one secret,
    /// when the same agent tries to reuse that token for another secret,
    /// then the map rejects the conflicting registration.
    #[test]
    fn placeholder_map_rejects_conflicting_token_registration() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let mut map = PlaceholderMap::default();

        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();
        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();

        assert_eq!(
            map.insert("agent-a", token, "OTHER_API_KEY"),
            Err(PlaceholderMapError::ConflictingToken {
                token: token.to_string()
            })
        );
    }

    /// Given a placeholder token is retired,
    /// when that same agent tries to resolve it again,
    /// then no secret name is returned.
    #[test]
    fn placeholder_map_remove_retires_token() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let identity = secrets_client::SecretAccessIdentity {
            agent_id: Some("agent-a".to_string()),
            ..Default::default()
        };
        let mut map = PlaceholderMap::default();

        map.insert("agent-a", token, "OPENAI_API_KEY").unwrap();

        assert!(map.remove("agent-a", token));
        assert_eq!(map.resolve(&identity, token), None);
        assert!(!map.remove("agent-a", token));
    }

    /// Given a set of placeholder tokens registered for the current agent,
    /// when resolve_tokens is called,
    /// then the returned map is keyed by full opaque token and values are
    /// authoritative secret names.
    #[test]
    fn placeholder_map_resolves_token_set_for_matching_agent() {
        let openai = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let db = "cfg_DATABASE-URL_ffffffffffffffffffffffffffffffff";
        let identity = secrets_client::SecretAccessIdentity {
            agent_id: Some("agent-a".to_string()),
            ..Default::default()
        };
        let mut map = PlaceholderMap::default();
        let tokens = HashSet::from([openai.to_string(), db.to_string()]);

        map.insert("agent-a", openai, "OPENAI_API_KEY").unwrap();
        map.insert("agent-a", db, "DATABASE_URL").unwrap();

        let resolved = map.resolve_tokens(&identity, &tokens).unwrap();

        assert_eq!(
            resolved.get(openai).map(String::as_str),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(resolved.get(db).map(String::as_str), Some("DATABASE_URL"));
    }

    /// Given a set containing an unregistered placeholder token,
    /// when resolve_tokens is called,
    /// then resolution fails closed instead of silently dropping it.
    #[test]
    fn placeholder_map_resolve_tokens_fails_closed_for_unknown_token() {
        let known = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let unknown = "cfg_DATABASE-URL_ffffffffffffffffffffffffffffffff";
        let identity = secrets_client::SecretAccessIdentity {
            agent_id: Some("agent-a".to_string()),
            ..Default::default()
        };
        let mut map = PlaceholderMap::default();
        let tokens = HashSet::from([known.to_string(), unknown.to_string()]);

        map.insert("agent-a", known, "OPENAI_API_KEY").unwrap();

        assert_eq!(
            map.resolve_tokens(&identity, &tokens),
            Err(PlaceholderResolutionError::UnknownToken(
                unknown.to_string()
            ))
        );
    }

    /// Given input with no valid placeholders,
    /// when substitute_placeholders is called,
    /// then it returns a borrowed Cow and leaves malformed cfg strings alone.
    #[test]
    fn substitute_placeholders_returns_borrowed_when_no_valid_placeholders() {
        let map = map_of(&[]);
        let input = "cfg_ cfg_short_deadbeef plain text";
        let result = substitute_placeholders(input, &map).unwrap();
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), input);
    }

    /// Given valid placeholder tokens and a map keyed by full token,
    /// when substitute_placeholders is called,
    /// then each placeholder is replaced and surrounding text is preserved.
    #[test]
    fn substitute_placeholders_replaces_full_opaque_tokens() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let other = "cfg_DATABASE-URL_ffffffffffffffffffffffffffffffff";
        let map = map_of(&[(token, "sk-real"), (other, "postgres://real")]);
        let input = format!("Authorization: Bearer {token}\nDatabase: {other}");
        let result = substitute_placeholders(&input, &map).unwrap();

        assert_eq!(
            result.as_ref(),
            "Authorization: Bearer sk-real\nDatabase: postgres://real"
        );
    }

    /// Given a valid placeholder token that is missing from the resolved map,
    /// when substitute_placeholders is called,
    /// then it fails closed instead of forwarding the opaque token upstream.
    #[test]
    fn substitute_placeholders_unresolvable_fails_closed() {
        let token = "cfg_OPENAI_KEY_0123456789abcdef0123456789abcdef";
        let map = map_of(&[]);
        let err = substitute_placeholders(token, &map).unwrap_err();
        assert_eq!(
            err,
            SubstitutionError::UnresolvablePlaceholder(token.to_string())
        );
    }

    // ── substitute ───────────────────────────────────────────────

    /// Given input with no refs,
    /// when substitute is called,
    /// then the result is Cow::Borrowed (zero allocation).
    #[test]
    fn substitute_returns_borrowed_when_no_refs() {
        let input = "https://api.example.com/path?q=v";
        let map = map_of(&[]);
        let result = substitute(input, &map).unwrap();
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "expected borrowed Cow, got owned"
        );
        assert_eq!(result.as_ref(), input);
    }

    /// Given a single well-formed ref and a map that contains it,
    /// when substitute is called,
    /// then the ref is replaced and surrounding text is preserved.
    #[test]
    fn substitute_single_ref_is_replaced() {
        let map = map_of(&[("FOO", "bar")]);
        let result = substitute("a-{{secret:FOO}}-b", &map).unwrap();
        assert_eq!(result.as_ref(), "a-bar-b");
    }

    /// Given input with multiple refs,
    /// when substitute is called,
    /// then each ref is replaced by the corresponding map value.
    #[test]
    fn substitute_multiple_refs() {
        let map = map_of(&[("A", "1"), ("B", "2")]);
        let result = substitute("a={{secret:A}}&b={{secret:B}}&c=3", &map).unwrap();
        assert_eq!(result.as_ref(), "a=1&b=2&c=3");
    }

    /// Given a ref whose name is NOT in the map,
    /// when substitute is called,
    /// then Unresolvable is returned with the exact missing name —
    /// fail-closed so the caller doesn't forward the literal to the
    /// upstream.
    #[test]
    fn substitute_unresolvable_fails_closed() {
        let map = map_of(&[("KNOWN", "v")]);
        let err = substitute("x={{secret:MISSING}}", &map).unwrap_err();
        assert_eq!(err, SubstitutionError::Unresolvable("MISSING".to_string()));
    }

    /// Given a map value that itself contains a `{{secret:X}}` string,
    /// when substitute is called,
    /// then the map value is inserted verbatim (NOT re-scanned). Pins
    /// the "single pass" contract.
    #[test]
    fn substitute_does_not_re_scan_resolver_output() {
        let map = map_of(&[("OUTER", "contains {{secret:INNER}}")]);
        let result = substitute("{{secret:OUTER}}", &map).unwrap();
        assert_eq!(result.as_ref(), "contains {{secret:INNER}}");
    }

    /// Given a nested ref,
    /// when substitute is called,
    /// then Nested is returned.
    #[test]
    fn substitute_rejects_nested() {
        let map = map_of(&[("FOO", "v")]);
        let err = substitute("{{secret:{{secret:FOO}}}}", &map).unwrap_err();
        assert_eq!(err, SubstitutionError::Nested);
    }

    /// Given UTF-8 multibyte characters around a ref,
    /// when substitute is called,
    /// then the multibyte chars are preserved.
    #[test]
    fn substitute_preserves_utf8() {
        let map = map_of(&[("X", "Y")]);
        let result = substitute("こんにちは-{{secret:X}}-世界", &map).unwrap();
        assert_eq!(result.as_ref(), "こんにちは-Y-世界");
    }

    /// Given an input containing only text with no refs AND no `{{`,
    /// when substitute is called,
    /// then the fast-path returns Borrowed. (Sanity check that the
    /// optimizer-branch for no-refs also triggers for inputs that
    /// happen to contain a single `{` but not `{{secret:`.)
    #[test]
    fn substitute_borrowed_for_inputs_with_single_brace() {
        let map = map_of(&[]);
        let result = substitute("one { brace not a ref", &map).unwrap();
        assert!(matches!(result, Cow::Borrowed(_)));
    }
}
