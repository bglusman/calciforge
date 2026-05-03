//! `[security.agent_web]` policy hooks.
//!
//! This module implements the four defenses described in
//! `crates/security-proxy/src/config.rs::AgentWebPolicy` and
//! `docs/security-gateway.md`:
//!
//! - **(A) Search-engine egress block** — refuse outbound requests to
//!   known search-API hosts entirely (when operators opt in).
//! - **(B) Search-response scanning** — scan responses from search APIs
//!   for prompt-injection AND for URLs that fail the destination
//!   denylist; either block the entire response or strip the offending
//!   entries.
//! - **(C) Provider-browsing strip/block** — recognise outbound
//!   chat-completions / messages bodies that include provider-side
//!   browsing tool defs or always-search models, and either rewrite
//!   the body to remove the tool defs or block.
//! - **(D) URL pre-flight** — extract URLs from outbound LLM message
//!   bodies / tool descriptions and refuse the request if any URL
//!   resolves to a denylisted host.
//!
//! All policy decisions are logged at INFO with structured fields
//! (`policy = "agent_web.<feature>"`, `dest_host`, `decision`, plus
//! tool/model/URL when relevant) so the audit pipeline picks them up.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use tracing::info;

use crate::config::AgentWebPolicy;

/// Returns true when `host` matches any pattern using the same matching
/// semantics as `bypass_domains` (`SecurityProxy::host_matches_pattern`):
///
///   - no `*`: host equals the pattern OR ends with `.<pattern>`
///     (DNS-label boundary — `notduckduckgo.com` does NOT match
///     `duckduckgo.com`)
///   - with `*`: glob; each `*` matches `[^.]*` (single label, no
///     dot-crossing) — so `*.corp.example` matches `a.corp.example`
///     but not `a.b.corp.example`.
///
/// All comparisons are case-insensitive.
pub fn host_matches_search_engine(host: &str, patterns: &[String]) -> bool {
    let h = host.to_ascii_lowercase();
    patterns.iter().any(|p| {
        let pl = p.to_ascii_lowercase();
        crate::proxy::SecurityProxy::host_matches_pattern(&h, &pl)
    })
}

/// Returns true when `host` is in the set of known LLM-provider hosts.
/// Uses the same wildcard-aware semantics as
/// [`host_matches_search_engine`] (and `bypass_domains`) so patterns
/// behave consistently across the gateway.
pub fn host_is_known_llm_api(host: &str, patterns: &[String]) -> bool {
    host_matches_search_engine(host, patterns)
}

/// Returns true when any URL-host extracted from `text` matches a deny
/// pattern from `patterns` (using the bypass-list-style suffix rule).
/// `denied_url_in_text` returns the offending host on the first hit so
/// callers can include it in audit logs / block reasons.
pub fn denied_url_in_text(text: &str, denylist: &[String]) -> Option<String> {
    if denylist.is_empty() {
        return None;
    }
    for url in extract_urls(text) {
        if let Some(host) = url_host(&url) {
            if host_matches_search_engine(&host, denylist) {
                return Some(host);
            }
        }
    }
    None
}

/// Extract all `http(s)://…` URLs from a free-text string.
///
/// Handles two encodings of the scheme separator that show up in the
/// real world:
///   - bare `https://` (text, headers, raw HTTP bodies)
///   - JSON-escaped `https:\/\/` (every JSON string emitter is allowed
///     by the spec to escape `/` as `\/`; some search APIs and OAuth
///     providers do this routinely). Without this branch, a denylisted
///     URL inside a JSON response slips past the regex and reaches the
///     model.
///
/// We canonicalise by removing all `\/` → `/` before regex matching, so
/// both encodings are captured by the same pattern. The extracted URL
/// is the unescaped form (callers that need byte-exact slicing into the
/// original buffer should re-find the match themselves; nothing in this
/// crate currently does).
pub fn extract_urls(text: &str) -> Vec<String> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"https?://[A-Za-z0-9._~:/?#\[\]@!$&'()*+,;=%-]+").unwrap());
    let canonical: String = if text.contains("\\/") {
        text.replace("\\/", "/")
    } else {
        // Hot path: no escapes present; avoid the allocation.
        // Borrowing `text` would require lifetime gymnastics with the
        // `Cow`; the small extra clone here keeps the function shape
        // simple and the regex iterator is on owned strings anyway.
        text.to_owned()
    };
    RE.find_iter(&canonical)
        .map(|m| m.as_str().to_owned())
        .collect()
}

fn url_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
}

/// (C) result of inspecting an outbound LLM request body for
/// provider-side browsing tools / always-search models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowsingDecision {
    /// No forbidden tools or models; forward unchanged.
    Allow,
    /// Body was rewritten to drop forbidden tool defs (`stripped` lists
    /// the names that were removed).
    Stripped {
        stripped: Vec<String>,
        body: Vec<u8>,
    },
    /// Block the request. `reason` is human-readable and safe to surface.
    Block { reason: String },
}

/// Inspect an outbound JSON body destined for an LLM provider. Returns
/// the policy decision. `dest_host` is used only for audit logging.
pub fn inspect_browsing_body(
    body: &[u8],
    policy: &AgentWebPolicy,
    dest_host: &str,
) -> BrowsingDecision {
    if !policy.forbid_provider_browsing {
        return BrowsingDecision::Allow;
    }
    let Ok(mut json) = serde_json::from_slice::<Value>(body) else {
        return BrowsingDecision::Allow;
    };

    // Always-search models — never strippable.
    if let Some(model) = json.get("model").and_then(Value::as_str) {
        if policy
            .forbidden_browsing_models
            .iter()
            .any(|p| model.starts_with(p.as_str()))
        {
            info!(
                policy = "agent_web.forbid_provider_browsing",
                dest_host = dest_host,
                model = model,
                decision = "block",
                "blocked LLM request: model is an always-search variant"
            );
            return BrowsingDecision::Block {
                reason: format!("model {model:?} performs built-in browsing"),
            };
        }
    }

    let mut stripped = Vec::new();
    if let Some(tools) = json.get_mut("tools").and_then(Value::as_array_mut) {
        let original_len = tools.len();
        tools.retain(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| tool.get("type").and_then(Value::as_str));
            let Some(name) = name else { return true };
            let forbidden = policy.forbidden_browsing_tools.iter().any(|t| t == name);
            if forbidden {
                stripped.push(name.to_owned());
                false
            } else {
                true
            }
        });
        if stripped.is_empty() && original_len > 0 {
            return BrowsingDecision::Allow;
        }
    } else if json.get("tools").is_none() {
        return BrowsingDecision::Allow;
    }

    if stripped.is_empty() {
        return BrowsingDecision::Allow;
    }

    match policy.provider_browsing_strategy.as_str() {
        "block" => {
            info!(
                policy = "agent_web.forbid_provider_browsing",
                dest_host = dest_host,
                tool = stripped.join(","),
                decision = "block",
                "blocked LLM request: forbidden provider-side browsing tool"
            );
            BrowsingDecision::Block {
                reason: "request used a forbidden provider-side browsing tool".into(),
            }
        }
        // "strip" or anything unknown defaults to strip (safer for
        // operators who miscapitalise the config).
        _ => {
            for name in &stripped {
                info!(
                    policy = "agent_web.forbid_provider_browsing",
                    dest_host = dest_host,
                    tool = name.as_str(),
                    decision = "strip",
                    "stripped browsing tool from LLM request"
                );
            }
            let body = serde_json::to_vec(&json).unwrap_or_else(|_| body.to_vec());
            BrowsingDecision::Stripped { stripped, body }
        }
    }
}

/// (D) Walk a JSON LLM-request body and collect every URL that appears
/// in `messages[].content` (string OR Anthropic content-array form),
/// and optionally in `tools[].description`. Returns the offending host
/// if any URL is on the denylist.
pub fn preflight_message_urls(body: &[u8], policy: &AgentWebPolicy) -> Option<String> {
    if !policy.preflight_message_urls {
        return None;
    }
    let Ok(json) = serde_json::from_slice::<Value>(body) else {
        return None;
    };

    let denylist = &policy.url_destination_denylist;
    if denylist.is_empty() {
        return None;
    }

    if let Some(messages) = json.get("messages").and_then(Value::as_array) {
        for msg in messages {
            let Some(content) = msg.get("content") else {
                continue;
            };
            if let Some(text) = content.as_str() {
                if let Some(host) = denied_url_in_text(text, denylist) {
                    return Some(host);
                }
            } else if let Some(parts) = content.as_array() {
                // Anthropic / tool-call content-array shape.
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if let Some(host) = denied_url_in_text(text, denylist) {
                            return Some(host);
                        }
                    }
                }
            }
        }
    }

    if policy.preflight_tool_descriptions {
        if let Some(tools) = json.get("tools").and_then(Value::as_array) {
            for tool in tools {
                if let Some(desc) = tool.get("description").and_then(Value::as_str) {
                    if let Some(host) = denied_url_in_text(desc, denylist) {
                        return Some(host);
                    }
                }
                // OpenAI shape: function.description nested
                if let Some(desc) = tool
                    .get("function")
                    .and_then(|f| f.get("description"))
                    .and_then(Value::as_str)
                {
                    if let Some(host) = denied_url_in_text(desc, denylist) {
                        return Some(host);
                    }
                }
            }
        }
    }

    None
}

/// (B) result of scanning a search-API response body.
#[derive(Debug, Clone)]
pub enum SearchResponseDecision {
    /// No denylisted URLs found; pass response through unchanged.
    Pass,
    /// Replace the response with a generic block page.
    Block { reason: String },
    /// Rewrite the response body, dropping entries whose URL was on the
    /// denylist.
    Strip {
        body: Vec<u8>,
        dropped_hosts: Vec<String>,
    },
}

/// Scan a search-API JSON response body. Looks for a `web.results` /
/// `results` / `organic` array of objects with a `url` (or `link`)
/// field; drops or blocks based on `policy.search_response_strategy`.
///
/// On JSON parse failure, falls back to "block" (safer than leaking).
pub fn scan_search_response(
    body: &[u8],
    policy: &AgentWebPolicy,
    dest_host: &str,
) -> SearchResponseDecision {
    if !policy.scan_search_responses {
        return SearchResponseDecision::Pass;
    }
    let denylist = &policy.url_destination_denylist;
    if denylist.is_empty() {
        return SearchResponseDecision::Pass;
    }

    // Quick cheap scan first: any denylisted URL anywhere in the bytes?
    let body_str = String::from_utf8_lossy(body);
    let Some(_first_hit) = denied_url_in_text(&body_str, denylist) else {
        return SearchResponseDecision::Pass;
    };

    if policy.search_response_strategy.as_str() == "strip" {
        match strip_denied_results(body, denylist) {
            Ok((rewritten, dropped)) if !dropped.is_empty() => {
                for host in &dropped {
                    info!(
                        policy = "agent_web.scan_search_responses",
                        dest_host = dest_host,
                        denied_host = host.as_str(),
                        decision = "strip",
                        "stripped denylisted entry from search response"
                    );
                }
                SearchResponseDecision::Strip {
                    body: rewritten,
                    dropped_hosts: dropped,
                }
            }
            // Either parse failed or strip couldn't locate the entries
            // — fail closed.
            _ => {
                info!(
                    policy = "agent_web.scan_search_responses",
                    dest_host = dest_host,
                    decision = "block",
                    "search response contained denylisted URL but strip \
                     failed to parse; failing closed to block"
                );
                SearchResponseDecision::Block {
                    reason: "search response referenced a forbidden URL".into(),
                }
            }
        }
    } else {
        info!(
            policy = "agent_web.scan_search_responses",
            dest_host = dest_host,
            decision = "block",
            "blocked search response: contained denylisted URL"
        );
        SearchResponseDecision::Block {
            reason: "search response referenced a forbidden URL".into(),
        }
    }
}

fn strip_denied_results(
    body: &[u8],
    denylist: &[String],
) -> Result<(Vec<u8>, Vec<String>), serde_json::Error> {
    let mut json: Value = serde_json::from_slice(body)?;
    let mut dropped = Vec::new();
    walk_and_strip(&mut json, denylist, &mut dropped);
    let bytes = serde_json::to_vec(&json)?;
    Ok((bytes, dropped))
}

/// Recursively walk a JSON value; in any array, drop elements whose
/// `url`/`link`/`href` field's host is on the denylist.
fn walk_and_strip(value: &mut Value, denylist: &[String], dropped: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            items.retain(|item| {
                let url = item
                    .get("url")
                    .or_else(|| item.get("link"))
                    .or_else(|| item.get("href"))
                    .and_then(Value::as_str);
                if let Some(url) = url {
                    if let Some(host) = url_host(url) {
                        if host_matches_search_engine(&host, denylist) {
                            dropped.push(host);
                            return false;
                        }
                    }
                }
                true
            });
            for item in items.iter_mut() {
                walk_and_strip(item, denylist, dropped);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                walk_and_strip(v, denylist, dropped);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_denylist(deny: &[&str]) -> AgentWebPolicy {
        AgentWebPolicy {
            url_destination_denylist: deny.iter().map(|s| (*s).to_string()).collect(),
            ..AgentWebPolicy::default()
        }
    }

    #[test]
    fn host_matches_search_engine_exact_and_suffix() {
        let pats: Vec<String> = vec!["api.search.brave.com".into(), "duckduckgo.com".into()];
        assert!(host_matches_search_engine("api.search.brave.com", &pats));
        assert!(host_matches_search_engine("html.duckduckgo.com", &pats));
        assert!(!host_matches_search_engine("brave.com", &pats));
        assert!(!host_matches_search_engine("evil.com", &pats));
    }

    #[test]
    fn extract_urls_finds_http_and_https() {
        let urls = extract_urls("see https://a.com/x and http://b.com");
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn denied_url_in_text_returns_offender() {
        let r = denied_url_in_text(
            "summarize https://blocked.example.com/path",
            &["blocked.example.com".into()],
        );
        assert_eq!(r.as_deref(), Some("blocked.example.com"));
    }

    #[test]
    fn preflight_finds_url_in_string_content() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "summarize https://blocked.example.com/x"}
            ]
        })
        .to_string();
        let policy = policy_with_denylist(&["blocked.example.com"]);
        assert_eq!(
            preflight_message_urls(body.as_bytes(), &policy).as_deref(),
            Some("blocked.example.com")
        );
    }

    #[test]
    fn preflight_finds_url_in_anthropic_content_array() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "summarize https://blocked.example.com/x"}
                ]}
            ]
        })
        .to_string();
        let policy = policy_with_denylist(&["blocked.example.com"]);
        assert_eq!(
            preflight_message_urls(body.as_bytes(), &policy).as_deref(),
            Some("blocked.example.com")
        );
    }

    #[test]
    fn preflight_skips_when_disabled() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "https://blocked.example.com"}]
        })
        .to_string();
        let mut policy = policy_with_denylist(&["blocked.example.com"]);
        policy.preflight_message_urls = false;
        assert!(preflight_message_urls(body.as_bytes(), &policy).is_none());
    }

    #[test]
    fn preflight_passes_clean_content() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "https://allowed.com"}]
        })
        .to_string();
        let policy = policy_with_denylist(&["blocked.example.com"]);
        assert!(preflight_message_urls(body.as_bytes(), &policy).is_none());
    }

    #[test]
    fn inspect_strips_openai_web_search_tool() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "tools": [
                {"type": "web_search", "name": "web_search"},
                {"type": "function", "name": "calc"}
            ]
        })
        .to_string();
        let policy = AgentWebPolicy {
            forbid_provider_browsing: true,
            ..AgentWebPolicy::default()
        };
        let decision = inspect_browsing_body(body.as_bytes(), &policy, "api.openai.com");
        match decision {
            BrowsingDecision::Stripped { stripped, body } => {
                assert_eq!(stripped, vec!["web_search"]);
                let parsed: Value = serde_json::from_slice(&body).unwrap();
                let tools = parsed["tools"].as_array().unwrap();
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["name"], "calc");
            }
            other => panic!("expected Stripped, got {other:?}"),
        }
    }

    #[test]
    fn inspect_blocks_when_strategy_block() {
        let body = serde_json::json!({
            "tools": [{"type": "web_search_20250305", "name": "web_search_20250305"}]
        })
        .to_string();
        let policy = AgentWebPolicy {
            forbid_provider_browsing: true,
            provider_browsing_strategy: "block".into(),
            ..AgentWebPolicy::default()
        };
        let decision = inspect_browsing_body(body.as_bytes(), &policy, "api.anthropic.com");
        assert!(matches!(decision, BrowsingDecision::Block { .. }));
    }

    #[test]
    fn inspect_blocks_search_model_even_with_strip_strategy() {
        let body = serde_json::json!({"model": "gpt-4o-search-preview"}).to_string();
        let policy = AgentWebPolicy {
            forbid_provider_browsing: true,
            provider_browsing_strategy: "strip".into(),
            ..AgentWebPolicy::default()
        };
        let decision = inspect_browsing_body(body.as_bytes(), &policy, "api.openai.com");
        assert!(matches!(decision, BrowsingDecision::Block { .. }));
    }

    #[test]
    fn inspect_allows_when_disabled() {
        let body = serde_json::json!({
            "tools": [{"type": "web_search", "name": "web_search"}]
        })
        .to_string();
        let policy = AgentWebPolicy::default(); // forbid_provider_browsing=false
        let decision = inspect_browsing_body(body.as_bytes(), &policy, "api.openai.com");
        assert_eq!(decision, BrowsingDecision::Allow);
    }

    #[test]
    fn scan_search_response_blocks_when_denied_url_present() {
        let body = serde_json::json!({
            "web": {"results": [{"url": "https://blocked.example.com/x", "title": "t"}]}
        })
        .to_string();
        let policy = policy_with_denylist(&["blocked.example.com"]);
        let decision = scan_search_response(body.as_bytes(), &policy, "api.search.brave.com");
        assert!(matches!(decision, SearchResponseDecision::Block { .. }));
    }

    #[test]
    fn scan_search_response_strips_denied_entries() {
        let body = serde_json::json!({
            "web": {"results": [
                {"url": "https://blocked.example.com/x", "title": "bad"},
                {"url": "https://allowed.example.com/y", "title": "good"}
            ]}
        })
        .to_string();
        let mut policy = policy_with_denylist(&["blocked.example.com"]);
        policy.search_response_strategy = "strip".into();
        let decision = scan_search_response(body.as_bytes(), &policy, "api.search.brave.com");
        match decision {
            SearchResponseDecision::Strip {
                body,
                dropped_hosts,
            } => {
                assert_eq!(dropped_hosts, vec!["blocked.example.com"]);
                let parsed: Value = serde_json::from_slice(&body).unwrap();
                let results = parsed["web"]["results"].as_array().unwrap();
                assert_eq!(results.len(), 1);
                assert_eq!(results[0]["title"], "good");
            }
            other => panic!("expected Strip, got {other:?}"),
        }
    }

    #[test]
    fn scan_search_response_passes_when_no_denylist_hit() {
        let body = serde_json::json!({"results": [{"url": "https://allowed.com"}]}).to_string();
        let policy = policy_with_denylist(&["blocked.example.com"]);
        let decision = scan_search_response(body.as_bytes(), &policy, "api.search.brave.com");
        assert!(matches!(decision, SearchResponseDecision::Pass));
    }
}
