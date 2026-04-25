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

use std::borrow::Cow;
use std::collections::HashSet;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubstitutionError {
    #[error("secret reference {{{{secret:{0}}}}} could not be resolved")]
    Unresolvable(String),

    #[error("nested secret references are not permitted")]
    Nested,

    #[error("malformed secret reference: {0}")]
    Malformed(String),
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
