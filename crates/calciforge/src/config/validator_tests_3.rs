use super::validator_test_support::{MIN_VALID, parse};
use super::*;

#[test]
fn removed_openclaw_http_agent_is_an_error() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-http"
endpoint = "http://127.0.0.1:18789"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(!result.is_valid(), "openclaw-http must fail validation");
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("openclaw-http") && e.contains("openclaw-channel")),
        "error should name the removed kind and migration target; errors: {:?}",
        result.errors
    );
}

#[test]
fn openclaw_channel_agent_validates_with_callback_auth() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key = "test-gateway-token"
reply_auth_token = "test-reply-token"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "openclaw-channel should validate; errors: {:?}",
        result.errors
    );
}

#[test]
fn openclaw_channel_agent_validates_with_callback_auth_file() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "custodian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key = "test-gateway-token"
reply_auth_token_file = "/tmp/calciforge-test-reply-token"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "openclaw-channel should validate; errors: {:?}",
        result.errors
    );
}

#[test]
fn openai_compat_agent_validates() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
model = "local-kimi-gpt55"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "openai-compat should validate; errors: {:?}",
        result.errors
    );
}

#[test]
fn openai_compat_rejects_openclaw_model_ids() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "librarian"
kind = "openai-compat"
endpoint = "http://127.0.0.1:18789"
api_key = "test-gateway-token"
model = "openclaw/main"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "OpenClaw model IDs should require openclaw-channel"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("OpenClaw") && e.contains("openclaw-channel")),
        "error should point to openclaw-channel; errors: {:?}",
        result.errors
    );
}

#[test]
fn openai_compat_without_model_requires_override_opt_in() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "openai-compat without model or allow_model_override must fail"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("allow_model_override")),
        "error should mention allow_model_override; errors: {:?}",
        result.errors
    );
}

#[test]
fn openai_compat_without_model_validates_when_override_is_explicit() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "gateway"
kind = "openai-compat"
endpoint = "http://127.0.0.1:8083"
api_key = "test-gateway-token"
allow_model_override = true
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "openai-compat with explicit model override should validate; errors: {:?}",
        result.errors
    );
}

#[test]
fn zeroclaw_agent_requires_api_key_or_file() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "librarianzero"
kind = "zeroclaw"
endpoint = "http://127.0.0.1:18799"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(!result.is_valid(), "zeroclaw without key must fail");
    assert!(
        result.errors.iter().any(|e| e.contains("api_key")),
        "error should mention missing api_key/api_key_file; errors: {:?}",
        result.errors
    );
}

#[test]
fn hermes_without_auth_fails_before_runtime() {
    let fixture = r#"
[calciforge]
version = 2

[[agents]]
id = "hermes"
kind = "hermes"
endpoint = "http://127.0.0.1:8642"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "Hermes config without auth should fail before adapter construction; errors: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("hermes") && error.contains("api_key")),
        "error should mention missing auth for Hermes; errors: {:?}",
        result.errors
    );
}

/// Given a config with two agents sharing the same id,
/// when validate_config runs,
/// then an error naming the duplicated id is produced.
#[test]
fn duplicate_agent_id_is_an_error() {
    let fixture = format!(
        "{MIN_VALID}\n[[agents]]\nid = \"bot\"\nkind = \"cli\"\ncommand = \"/bin/echo\"\nargs = []\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(!result.is_valid(), "duplicate agent id must fail");
    assert!(
        result.errors.iter().any(|e| e.contains("bot")),
        "error must name the duplicated id 'bot'; errors: {:?}",
        result.errors
    );
}

#[test]
fn agent_alias_cannot_shadow_another_agent_id() {
    let fixture = format!(
        "{MIN_VALID}\n[[agents]]\nid = \"helper\"\nkind = \"cli\"\ncommand = \"/bin/echo\"\nargs = []\naliases = [\"bot\"]\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "agent aliases must not silently shadow another agent id"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("Ambiguous agent selector 'bot'")
                && e.contains("agent 'bot'")
                && e.contains("agent 'helper'")
        }),
        "error should identify both agent selector owners; errors: {:?}",
        result.errors
    );
}

/// Given a config with two identities sharing the same id,
/// when validate_config runs,
/// then an error naming the duplicated id is produced.
#[test]
fn duplicate_identity_id_is_an_error() {
    let fixture = format!(
        "{MIN_VALID}\n[[identities]]\nid = \"alice\"\naliases = [{{ channel = \"signal\", id = \"7000000099\" }}]\nrole = \"user\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(!result.is_valid(), "duplicate identity id must fail");
    assert!(
        result.errors.iter().any(|e| e.contains("alice")),
        "error must name the duplicated id 'alice'; errors: {:?}",
        result.errors
    );
}

/// Given a proxy config with an unparseable bind address,
/// when validate_config runs,
/// then an error is produced naming the bind/address problem.
#[test]
fn malformed_proxy_bind_is_an_error() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"not-an-address\"\nbackend_type = \"http\"\nbackend_url = \"https://api.example.com\"\ntimeout_seconds = 10\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "invalid bind address must fail; errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| {
            let lower = e.to_lowercase();
            lower.contains("bind") || lower.contains("address")
        }),
        "error should name the bind/address problem; errors: {:?}",
        result.errors
    );
}

/// Given a proxy config with `timeout_seconds = 0`,
/// when validate_config runs,
/// then an error is produced — a zero timeout means requests
/// hang indefinitely, which is never the intent.
#[test]
fn zero_proxy_timeout_is_an_error() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"http\"\nbackend_url = \"https://api.example.com\"\ntimeout_seconds = 0\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "zero proxy timeout must fail; errors: {:?}",
        result.errors
    );
}

/// Given a proxy config with a gateway UI link that chat users may open,
/// when validate_config runs,
/// then non-HTTP links are rejected before they appear in `!help`.
#[test]
fn gateway_ui_url_requires_http_url() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"http\"\nbackend_url = \"https://api.example.com\"\ngateway_ui_url = \"localhost:8585\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "gateway UI URL without scheme must fail; errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| e.contains("gateway_ui_url")),
        "error should name gateway_ui_url; errors: {:?}",
        result.errors
    );
}

/// Given a proxy backend type outside the runtime allow-list,
/// when validate_config runs,
/// then validation rejects it and reports the supported values.
#[test]
fn unsupported_proxy_backend_type_is_rejected_by_allowlist() {
    let backend_type = "experimental-gateway";
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"{backend_type}\"\nbackend_url = \"https://api.example.com\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "{backend_type} must not validate as a supported proxy backend; errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("backend_type") && e.contains(backend_type) && e.contains("http")
        }),
        "error should name unsupported backend and supported values; errors: {:?}",
        result.errors
    );
}

#[test]
fn named_openai_compatible_backend_types_are_validated_from_shared_allowlist() {
    for backend_type in [
        "litellm",
        "portkey",
        "tensorzero",
        "future-agi",
        "openrouter",
    ] {
        let fixture = format!(
            "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"{backend_type}\"\nbackend_url = \"https://gateway.example.invalid/v1\"\n"
        );
        let config = parse(&fixture);
        let result = validate_config(&config);
        assert!(
            result.is_valid(),
            "{backend_type} should be accepted as a supported OpenAI-compatible provider adapter; errors: {:?}",
            result.errors
        );
    }
}

/// Given a disabled proxy with a configured gateway UI link,
/// when validate_config runs,
/// then the UI URL is still validated because chat help can surface it from
/// config even when the model gateway listener is disabled.
#[test]
fn disabled_proxy_still_validates_gateway_ui_url() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = false\ngateway_ui_url = \"javascript:alert(1)\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "disabled proxy must still validate displayed gateway UI URLs; errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| e.contains("gateway_ui_url")),
        "error should name gateway_ui_url; errors: {:?}",
        result.errors
    );
}

/// Given an OpenAI-compatible provider adapter is selected,
/// when the backend URL is malformed or contains request modifiers,
/// then validation rejects it before runtime path construction can fail.
#[test]
fn provider_adapter_backend_url_must_be_plain_http_base_url() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"litellm\"\nbackend_url = \"https://litellm.example.invalid/v1?debug=true\"\nbackend_api_key = \"test-key\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "provider adapter backend URL with query string must fail; errors: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| { e.contains("Proxy backend_url") && e.contains("query") }),
        "error should name Proxy backend_url and query/fragment issue; errors: {:?}",
        result.errors
    );
}

/// Given Helicone is selected for a likely local unauthenticated gateway,
/// when no backend key is configured,
/// then validation warns instead of silently accepting a surprising empty
/// `Authorization: Bearer` header.
#[test]
fn helicone_without_backend_key_warns_operator() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"helicone\"\nbackend_url = \"http://127.0.0.1:8787\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "local unauthenticated Helicone should remain possible: {:?}",
        result.errors
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("Helicone backend has no backend_api_key")),
        "missing Helicone key should produce an operator warning; warnings: {:?}",
        result.warnings
    );
}

/// Given a routing rule referencing an agent id that doesn't
/// exist in the agent list,
/// when validate_config runs,
/// then an error naming the missing agent is produced.
///
/// Catches the most common cause of silent "agent unavailable"
/// at runtime: typo in a routing rule.
#[test]
fn routing_rule_default_to_nonexistent_agent_is_an_error() {
    let fixture =
        format!("{MIN_VALID}\n[[routing]]\nidentity = \"alice\"\ndefault_agent = \"ghost\"\n");
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "routing default_agent pointing at a non-existent agent must fail; \
             errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| e.contains("ghost")),
        "error should name the missing agent id 'ghost'; errors: {:?}",
        result.errors
    );
}

/// Given a routing rule whose `default_agent` is valid but whose
/// `allowed_agents` list contains an id not in the agent list,
/// when validate_config runs,
/// then an error naming both the identity and the missing agent
/// is produced.
///
/// Validates the branch in `validate_routing_rules` that walks
/// each entry of `allowed_agents` — a test for
/// `default_agent` alone wouldn't exercise this code path.
#[test]
fn routing_rule_allowed_list_with_nonexistent_agent_is_an_error() {
    let fixture = format!(
        "{MIN_VALID}\n[[routing]]\nidentity = \"alice\"\ndefault_agent = \"bot\"\nallowed_agents = [\"bot\", \"ghost\"]\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "allowed_agents pointing at a non-existent agent must fail; \
             errors: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("ghost") && e.contains("alice")),
        "error should name both the identity 'alice' and missing agent \
             'ghost'; errors: {:?}",
        result.errors
    );
}
