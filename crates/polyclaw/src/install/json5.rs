//! JSON5 / JSONC comment stripping for PolyClaw's installer.
//!
//! This module provides the **correct** implementation of `strip_json_comments`
//! and `parse_json5_relaxed`, copied from the canonical NZC implementation in
//! `crates/nonzeroclaw/src/onboard/migration.rs`.
//!
//! # Why a separate copy?
//!
//! `polyclaw` does not depend on `nonzeroclaw` (to avoid circular crate
//! dependencies and to keep polyclaw independently distributable).  Until a
//! shared `claw-types` crate is extracted, this module is the authoritative
//! copy for use within `polyclaw`.
//!
//! # TODO (follow-on)
//!
//! Extract both this and NZC's copy to a shared `claw-types` crate so there
//! is exactly one implementation.  See `docs/opus-review.md` D1 for context.
//!
//! # Bug fixed vs old executor.rs copy
//!
//! The old `strip_json_comments_simple` in `executor.rs` used a `prev`
//! character to detect escaped quotes, which incorrectly handled `"\\"` (an
//! escaped backslash before a closing quote).  This implementation uses an
//! `escape_next` boolean flag instead, which correctly handles all single-
//! character escape sequences including `\\`, `\"`, etc.

use anyhow::{Context, Result};

/// Parse a JSON5 / JSONC string by stripping line comments (`// ...`) and
/// block comments (`/* ... */`) before handing off to `serde_json`.
///
/// This is intentionally simple: it handles the common cases in OpenClaw's
/// config without pulling in a full JSON5 parser.  Edge cases (e.g. `//`
/// inside a string literal) are handled correctly via the `escape_next` flag.
pub fn parse_json5_relaxed(input: &str) -> Result<serde_json::Value> {
    let stripped = strip_json_comments(input);
    serde_json::from_str(&stripped).context("JSON parse failed after stripping comments")
}

/// Strip `// line` and `/* block */` comments from a JSON-like string.
///
/// Correctly handles all single-character escape sequences inside strings,
/// including `\\` (escaped backslash) followed by `"` (quote).
pub fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;
    let mut escape_next = false;

    while i < len {
        let ch = chars[i];

        if escape_next {
            // Character after a backslash: always emit it verbatim, never
            // treat it as a string terminator or comment starter.
            out.push(ch);
            escape_next = false;
            i += 1;
            continue;
        }

        if in_string {
            if ch == '\\' {
                // Next character is escaped — set the flag and emit the backslash.
                escape_next = true;
                out.push(ch);
            } else if ch == '"' {
                // Unescaped quote: end of string.
                in_string = false;
                out.push(ch);
            } else {
                out.push(ch);
            }
            i += 1;
            continue;
        }

        // Not in a string.
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
                i += 1;
            }
            '/' if i + 1 < len && chars[i + 1] == '/' => {
                // Line comment — skip to end of line (preserve the newline).
                i += 2;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < len && chars[i + 1] == '*' => {
                // Block comment — skip to `*/`.
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                if i + 1 < len {
                    i += 2; // skip `*/`
                }
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comments() {
        let input = r#"{ "key": "value" // line comment
}"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strips_block_comments() {
        let input = r#"{ /* block */ "key": "value" }"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn preserves_url_in_string() {
        // `//` inside a string must NOT be treated as a comment.
        let input = r#"{ "url": "http://example.com/path" }"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["url"], "http://example.com/path");
    }

    /// Regression test for the escaped-backslash bug in the old implementation.
    ///
    /// `"\\"` is a JSON string containing a single backslash.  The closing `"`
    /// is NOT escaped — it ends the string.  The old `prev`-based approach
    /// incorrectly treated it as an escaped quote.
    #[test]
    fn escaped_backslash_before_closing_quote() {
        // JSON: {"k": "\\"} — value is a single backslash
        let input = r#"{"k": "\\"}"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["k"], "\\");
    }

    #[test]
    fn escaped_quote_inside_string() {
        // JSON: {"k": "he said \"hi\""} — value contains quotes
        let input = r#"{"k": "he said \"hi\""}"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["k"], "he said \"hi\"");
    }

    #[test]
    fn comment_after_escaped_backslash_string() {
        // Tricky: value ends with `\\`, then there's a line comment.
        let input = "{ \"k\": \"path\\\\\" // comment\n}";
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        // value is "path\\" (path with one trailing backslash)
        assert_eq!(v["k"], "path\\");
    }

    #[test]
    fn empty_input_returns_error() {
        assert!(parse_json5_relaxed("").is_err());
    }

    #[test]
    fn plain_json_unchanged() {
        let input = r#"{"a": 1, "b": "two", "c": true}"#;
        let v: serde_json::Value = parse_json5_relaxed(input).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "two");
        assert_eq!(v["c"], true);
    }

    // ── Property tests (hegel) ────────────────────────────────────────────────

    /// Property: `strip_json_comments` is idempotent.
    ///
    /// For any input string, `strip(strip(s)) == strip(s)`.
    ///
    /// This is a strong property: it rules out a whole class of bugs where
    /// the first pass leaves behind partial comment markers or accidentally
    /// introduces new `//` sequences.  Idempotency must hold for ALL inputs —
    /// not just valid JSON.
    ///
    /// This is non-trivial: a buggy implementation could, for example,
    /// transform `/*` into something that becomes a comment on the second pass.
    #[hegel::test]
    fn prop_strip_json_comments_idempotent(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Use arbitrary text — not just valid JSON.  The idempotency property
        // must hold for all strings, including adversarial ones.
        let input = tc.draw(gs::text().max_size(200));

        let once = strip_json_comments(&input);
        let twice = strip_json_comments(&once);

        assert_eq!(
            once, twice,
            "strip_json_comments is not idempotent!\n\
             input:  {:?}\n\
             once:   {:?}\n\
             twice:  {:?}",
            input, once, twice
        );
    }

    /// Property: `strip_json_comments` never increases length.
    ///
    /// Comment stripping only removes characters, never adds them.
    /// `strip(s).len() <= s.len()` for all inputs.
    ///
    /// Would catch bugs where the state machine emits extra characters (e.g.
    /// an unmatched `/*` at end of input triggers a spurious character emit).
    #[hegel::test]
    fn prop_strip_json_comments_never_adds_content(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        let input = tc.draw(gs::text().max_size(500));
        let stripped = strip_json_comments(&input);

        // Output length in bytes must not exceed input length.
        assert!(
            stripped.len() <= input.len(),
            "strip_json_comments added content!\n\
             input.len()={} stripped.len()={}\n\
             input:   {:?}\n\
             stripped:{:?}",
            input.len(),
            stripped.len(),
            input,
            stripped
        );
    }

    /// Property: `strip_json_comments` preserves valid JSON structure.
    ///
    /// For any valid JSON string `s` (no comments, no special chars that would
    /// confuse our state machine), `parse_json5_relaxed(s)` produces the same
    /// value as `serde_json::from_str(s)`.
    ///
    /// This is the "round-trip" property from opus-review-2.md §1: stripping
    /// comments from comment-free JSON must not change the parsed value.
    ///
    /// Uses strings restricted to avoid embedded quotes/backslashes in order
    /// to generate inputs where both parsers should agree.
    #[hegel::test]
    fn prop_strip_json_comments_preserves_valid_json(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Generate a simple key that is safe to embed in JSON.
        let key = tc.draw(
            gs::text()
                .min_size(1)
                .max_size(30)
                .filter(|s: &String| {
                    // No characters that would interact with JSON escaping or
                    // the comment-stripping state machine.
                    !s.contains('"')
                        && !s.contains('\\')
                        && !s.contains("//")
                        && !s.contains("/*")
                        && s.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
                }),
        );

        let json = format!(r#"{{"k": "{}"}}"#, key);

        // Direct parse must succeed (we built valid JSON).
        let direct: serde_json::Value = serde_json::from_str(&json)
            .expect("test-generated JSON must be valid");

        // Parse via strip_json_comments must yield the same value.
        let via_strip: serde_json::Value = parse_json5_relaxed(&json)
            .expect("stripping comments on valid JSON must not break it");

        assert_eq!(
            direct, via_strip,
            "strip_json_comments changed the parsed value!\n\
             input: {:?}",
            json
        );
    }
}
