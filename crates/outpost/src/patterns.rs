//! Compiled pattern sets for structural and semantic scanning.
//!
//! All patterns are compiled once at process startup via `once_cell::sync::Lazy`
//! and reused across all scans.

use aho_corasick::AhoCorasick;
use once_cell::sync::Lazy;
use regex::Regex;

// ── Layer 1: Structural patterns ──────────────────────────────────────────────

/// Zero-width and invisible Unicode characters used to hide content.
pub static RE_ZERO_WIDTH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\u{200B}\u{200C}\u{200D}\u{2060}\u{00AD}]").expect("RE_ZERO_WIDTH compile")
});

/// Unicode tag characters (U+E0000–U+E007F) — invisible but model-visible.
pub static RE_UNICODE_TAGS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\u{E0000}-\u{E007F}]").expect("RE_UNICODE_TAGS compile"));

/// CSS-based content-hiding patterns.
pub static RE_CSS_HIDING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        display \s* : \s* none                |
        visibility \s* : \s* hidden           |
        font-size \s* : \s* 0 (\s*px)?        |
        opacity \s* : \s* 0                   |
        color \s* : \s* (white | \#fff(fff)?) |
        color \s* : \s* rgba? \s* \( \s* 255 \s* , \s* 255 \s* , \s* 255",
    )
    .expect("RE_CSS_HIDING compile")
});

/// Large base64-encoded blobs (> 500 consecutive base64 chars).
pub static RE_BASE64_BLOB: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[A-Za-z0-9+/\-_]{500,}={0,2}").expect("RE_BASE64_BLOB compile"));

// ── Layer 2: Semantic patterns ─────────────────────────────────────────────────

/// Direct instruction-override phrases targeting language models.
pub static AC_INJECTION_PHRASES: Lazy<AhoCorasick> = Lazy::new(|| {
    AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build([
            "ignore previous instructions",
            "ignore all previous instructions",
            "disregard previous instructions",
            "disregard all previous",
            "disregard the above",
            "forget previous instructions",
            "forget all previous",
            "you are now",
            "act as if you are",
            "act as a",
            "pretend you are",
            "pretend to be",
            "roleplay as",
            "jailbreak",
            "DAN mode",
            "developer mode enabled",
            "ignore your instructions",
            "override your instructions",
            "your new instructions are",
            "new system prompt",
            "ignore the system prompt",
            "bypass your",
            "you have no restrictions",
            "you have no rules",
            "unlimited mode",
        ])
        .expect("AC_INJECTION_PHRASES compile")
});

/// PII harvesting patterns.
pub static RE_PII_HARVEST: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        (send | give | provide | share | reveal | disclose | tell \s+ me | what \s+ is)
        \s+ (me \s+)?
        (your | the)? \s*
        (password | api[-_\s]?key | secret[-_\s]?key | auth[-_\s]?token | access[-_\s]?token |
         credential | private[-_\s]?key | ssh[-_\s]?key | bearer[-_\s]?token |
         two[-_\s]?factor | 2fa | otp | recovery[-_\s]?code)",
    )
    .expect("RE_PII_HARVEST compile")
});

/// Exfiltration signal patterns.
pub static RE_EXFILTRATION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        exfiltrate                                    |
        POST \s+ to \s+ https?://                     |
        send \s+ to \s+ https?://                     |
        report \s+ back \s+ to                        |
        beacon \s+ to                                 |
        (curl|wget|nc|netcat) \s+ .{0,80} https?://",
    )
    .expect("RE_EXFILTRATION compile")
});

/// Discussion-context keywords — signal that content is ABOUT injection rather
/// than attempting it. Used to suppress false positives in security research.
pub static AC_DISCUSSION_CONTEXT: Lazy<AhoCorasick> = Lazy::new(|| {
    AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build([
            "prompt injection",
            "jailbreak attempt",
            "adversarial prompt",
            "this is an example",
            "example of injection",
            "how attackers",
            "researchers have found",
            "security researchers",
            "cve-",
            "vulnerability",
            "proof of concept",
            "poc exploit",
        ])
        .expect("AC_DISCUSSION_CONTEXT compile")
});

/// Count discussion-context signals in `text`. Used to compute the discussion
/// ratio heuristic: if > 30% of injection phrase matches are accompanied by
/// discussion context, treat the content as review rather than unsafe.
pub fn count_discussion_signals(text: &str) -> usize {
    AC_DISCUSSION_CONTEXT.find_iter(text).count()
}

/// Count injection phrase matches in `text`.
pub fn count_injection_signals(text: &str) -> usize {
    AC_INJECTION_PHRASES.find_iter(text).count()
}
