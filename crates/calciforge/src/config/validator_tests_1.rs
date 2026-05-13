use super::validator_test_support::{MIN_VALID, parse};
use super::*;

/// Given a minimal config with no violations,
/// when validate_config runs,
/// then `is_valid()` is true. Positive baseline.
#[test]
fn baseline_minimum_config_validates_clean() {
    let config = parse(MIN_VALID);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "baseline fixture should validate clean; errors: {:?}",
        result.errors
    );
}

#[test]
fn security_profile_validation_matches_runtime_parser() {
    let invalid = parse(&format!("{MIN_VALID}\n[security]\nprofile = \"minimal\"\n"));
    let invalid_result = validate_config(&invalid);
    assert!(
        invalid_result
            .errors
            .iter()
            .any(|error| error.contains("Security profile 'minimal' is invalid")),
        "unsupported profile must fail validation before runtime fallback; errors: {:?}",
        invalid_result.errors
    );

    let maximum = parse(&format!("{MIN_VALID}\n[security]\nprofile = \"maximum\"\n"));
    let maximum_result = validate_config(&maximum);
    assert!(
        maximum_result.is_valid(),
        "maximum is a runtime-supported alias for paranoid; errors: {:?}",
        maximum_result.errors
    );
}

#[test]
fn model_shortcut_cycles_are_config_errors() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[model_shortcuts]]
alias = "local"
model = "balanced"

[[model_shortcuts]]
alias = "balanced"
model = "local"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "cyclic model aliases should fail validation before runtime"
    );
    assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("model shortcut cycle")
                    && e.contains("local -> balanced -> local")),
            "error should identify the shortcut cycle; errors: {:?}",
            result.errors
        );
}

#[test]
fn model_roles_share_shortcut_resolution_and_cycle_checks() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[model_roles]]
role = "fast"
model = "balanced"

[[model_shortcuts]]
alias = "balanced"
model = "fast"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "roles must share shortcut cycle validation"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("model shortcut cycle") && e.contains("fast -> balanced -> fast")),
        "error should identify role/shortcut cycle; errors: {:?}",
        result.errors
    );
}

#[test]
fn duplicate_model_role_and_shortcut_names_are_config_errors() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[model_roles]]
role = "security.screening"
model = "local/qwen"

[[model_shortcuts]]
alias = "security.screening"
model = "cloud/gpt"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "roles and shortcuts share one public selector namespace"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("Duplicate model shortcut alias or role")
                && e.contains("security.screening")),
        "error should identify duplicate role/shortcut name; errors: {:?}",
        result.errors
    );
}

#[test]
fn model_shortcut_alias_cannot_shadow_synthetic_model_id() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[dispatchers]]
id = "balanced"

[[dispatchers.models]]
model = "qwen-test:small"
context_window = 60000

[[model_shortcuts]]
alias = "balanced"
model = "qwen-test:small"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "shortcut aliases must not silently shadow synthetic model IDs"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("balanced")
            && e.contains("Ambiguous model shortcut alias")
            && e.contains("synthetic model ID")),
        "error should identify the colliding alias and synthetic ID; errors: {:?}",
        result.errors
    );
}

#[test]
fn model_shortcut_alias_cannot_shadow_local_model_id() {
    let fixture = format!(
        r#"
{MIN_VALID}

[local_models]
enabled = true

[[local_models.models]]
id = "local"
hf_id = "example/local"

[[model_shortcuts]]
alias = "local"
model = "qwen-test:small"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "shortcut aliases must not silently shadow local model IDs"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("local")
                && e.contains("Ambiguous model shortcut alias")
                && e.contains("local model ID")
        }),
        "error should identify the colliding alias and local model ID; errors: {:?}",
        result.errors
    );
}

#[test]
fn model_shortcut_alias_cannot_shadow_exact_provider_model_id() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[proxy.providers]]
id = "remote"
backend_type = "http"
url = "https://example.invalid/v1"
models = ["openai/gpt-5.5", "openai/*"]

[[model_shortcuts]]
alias = "openai/gpt-5.5"
model = "kimi-cli"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "shortcut aliases must not silently shadow exact provider model IDs"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("openai/gpt-5.5")
                && e.contains("Ambiguous model shortcut alias")
                && e.contains("provider model ID")
        }),
        "error should identify the colliding alias and provider model ID; errors: {:?}",
        result.errors
    );
}

#[test]
fn provider_strip_model_prefix_must_not_be_empty() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "opencode-go"
backend_type = "http"
url = "https://opencode.ai/zen/go/v1"
models = ["opencode-go/kimi-k2.6"]
strip_model_prefix = " "
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "empty strip_model_prefix should fail validation"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("strip_model_prefix cannot be empty")),
        "error should mention strip_model_prefix; errors: {:?}",
        result.errors
    );
}

#[test]
fn provider_strip_model_prefix_warns_when_no_models_use_it() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "opencode-go"
backend_type = "http"
url = "https://opencode.ai/zen/go/v1"
api_key = "test-provider-key"
models = ["kimi-k2.6"]
strip_model_prefix = "opencode-go/"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "mismatched strip prefix should warn, not block unrelated direct models"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("strips model prefix")),
        "warning should identify useless strip_model_prefix; warnings: {:?}",
        result.warnings
    );
}

#[test]
fn gateway_retry_config_rejects_invalid_backoff_bounds() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[proxy.retry]
enabled = true
min_timeout_ms = 2000
max_timeout_ms = 1000
factor = 0
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "invalid retry timing should fail validation before runtime"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("min_timeout_ms") && e.contains("max_timeout_ms")),
        "error should identify inverted retry bounds; errors: {:?}",
        result.errors
    );
    assert!(
        result.errors.iter().any(|e| e.contains("factor")),
        "error should reject zero retry factor; errors: {:?}",
        result.errors
    );
}

#[test]
fn provider_owned_provider_key_is_endpoint_auth_warning_not_error() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "managed-gateway"
backend_type = "http"
url = "http://127.0.0.1:4000/v1"
model_credential_owner = "provider"
api_key = "sk-local-gateway-client"
models = ["gateway/default"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "provider-owned provider keys should be a supported config shape; errors: {:?}",
        result.errors
    );
    assert!(
        result.warnings.iter().any(|w| {
            w.contains("managed-gateway")
                && w.contains("model_credential_owner='provider'")
                && w.contains("provider boundary endpoint")
        }),
        "warning should clarify api_key is provider-boundary transport auth; warnings: {:?}",
        result.warnings
    );
    assert!(
        result.warnings.iter().any(|w| {
            w.contains("managed-gateway")
                && w.contains("provider-owned OpenAI-compatible boundary")
                && w.contains("LiteLLM")
        }),
        "warning should distinguish provider-owned HTTP endpoints from raw upstream providers; warnings: {:?}",
        result.warnings
    );
}

#[test]
fn builtin_http_provider_warns_that_it_is_not_provider_owned_boundary() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true
backend_type = "helicone"
backend_url = "http://127.0.0.1:8787/ai"

[[proxy.providers]]
id = "raw-upstream"
backend_type = "http"
url = "https://example.invalid/v1"
api_key = "test-provider-key"
models = ["raw/model"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "builtin HTTP providers remain supported but should be explicit; errors: {:?}",
        result.errors
    );
    assert!(
        result.warnings.iter().any(|w| {
            w.contains("raw-upstream")
                && w.contains("builtin HTTP upstream adapter")
                && w.contains("not a provider-owned boundary")
        }),
        "warning should prevent treating raw HTTP provider routes as equal to Helicone/LiteLLM; warnings: {:?}",
        result.warnings
    );
}

#[test]
fn mock_proxy_backend_warns_for_real_deployments() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true
backend_type = "mock"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(!result.is_valid(), "mock without providers must fail now");
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("mock provider adapter")),
        "error should make mock/no-provider invalid; errors: {:?}",
        result.errors
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("backend_type='mock'") && w.contains("test-only")),
        "warning should keep mock out of production configs; warnings: {:?}",
        result.warnings
    );
}

#[test]
fn explicit_providers_do_not_make_blank_http_root_valid() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true
backend_type = "http"
backend_url = ""

[[proxy.providers]]
id = "managed"
backend_type = "http"
url = "http://127.0.0.1:4000/v1"
model_credential_owner = "provider"
models = ["managed/default"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "blank root HTTP backend should be invalid even when providers are configured"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("backend_type='http'") && e.contains("requires backend_url")),
        "error should identify blank root backend_url; errors: {:?}",
        result.errors
    );
}
