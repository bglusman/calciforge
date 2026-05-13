use super::validator_test_support::{MIN_VALID, parse};
use super::*;

#[test]
fn calciforge_owned_provider_requires_key_or_file() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "raw-upstream"
backend_type = "http"
url = "https://example.invalid/v1"
model_credential_owner = "calciforge"
models = ["raw/model"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "Calciforge-owned provider credentials must be explicit"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("raw-upstream")
                && e.contains("model_credential_owner='calciforge'")
                && e.contains("model_api_key/model_api_key_file")
        }),
        "error should identify provider and missing credential; errors: {:?}",
        result.errors
    );
}

#[test]
fn calciforge_owned_provider_accepts_model_key_without_endpoint_key() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "raw-upstream"
backend_type = "http"
url = "https://example.invalid/v1"
model_credential_owner = "calciforge"
model_api_key = "upstream-model-key"
models = ["raw/model"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "separate provider-auth and model-auth credentials should be valid; errors: {:?}",
        result.errors
    );
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| w.contains("legacy api_key/api_key_file as both")),
        "explicit model credentials should avoid legacy dual-use warning; warnings: {:?}",
        result.warnings
    );
}

#[test]
fn calciforge_owned_provider_rejects_two_bearer_credentials() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "raw-upstream"
backend_type = "http"
url = "https://example.invalid/v1"
api_key = "endpoint-virtual-key"
model_credential_owner = "calciforge"
model_api_key = "upstream-model-key"
models = ["raw/model"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "current provider adapters cannot send two independent bearer credentials"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("raw-upstream")
                && e.contains("both provider adapter auth")
                && e.contains("one bearer credential")
        }),
        "error should identify the two-credential conflict; errors: {:?}",
        result.errors
    );
}

#[test]
fn provider_owned_model_credentials_reject_model_key() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "managed"
backend_type = "http"
url = "https://managed.example.invalid/v1"
api_key = "adapter-client-key"
model_credential_owner = "provider"
model_api_key = "wrong-place"
models = ["managed/default"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "provider-owned final model credentials must not also configure model_api_key"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("managed") && e.contains("model_api_key")),
        "error should identify provider-owned/model-key conflict; errors: {:?}",
        result.errors
    );
}

#[test]
fn deprecated_model_credential_owner_none_aliases_to_provider() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true

[[proxy.providers]]
id = "local"
backend_type = "http"
url = "http://127.0.0.1:11434/v1"
model_credential_owner = "none"
models = ["ollama/qwen3.6:27b"]
"#
    );
    let config = parse(&fixture);
    let owner = config
        .proxy
        .as_ref()
        .and_then(|proxy| proxy.providers.first())
        .map(|provider| provider.model_credential_owner);
    assert_eq!(owner, Some(CredentialOwner::Provider));

    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "deprecated model_credential_owner='none' should parse as provider-owned/no Calciforge model credentials"
    );
}

#[test]
fn model_shortcut_alias_cannot_shadow_exact_model_route_id() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[proxy.providers]]
id = "remote"
backend_type = "http"
url = "https://example.invalid/v1"

[[proxy.model_routes]]
pattern = "coding/default"
provider = "remote"

[[model_shortcuts]]
alias = "coding/default"
model = "kimi-cli"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "shortcut aliases must not silently shadow exact model-route IDs"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("coding/default")
                && e.contains("Ambiguous model shortcut alias")
                && e.contains("model-route ID")
        }),
        "error should identify the colliding alias and model-route ID; errors: {:?}",
        result.errors
    );
}

#[test]
fn exact_model_route_cannot_shadow_agent_selector() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[agents]]
id = "local-dispatcher"
kind = "openai-compat"
endpoint = "http://127.0.0.1:18083"
model = "local-cloud-coding"

[[proxy.providers]]
id = "remote"
backend_type = "http"
url = "https://example.invalid/v1"

[[proxy.model_routes]]
pattern = "local-dispatcher"
provider = "remote"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "exact model routes must not silently treat agent names as concrete gateway models"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("local-dispatcher") && e.contains("model-route ID") && e.contains("agent ID")
        }),
        "error should identify the model route / agent selector collision; errors: {:?}",
        result.errors
    );
}

#[test]
fn provider_model_id_cannot_shadow_agent_alias() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[agents]]
id = "gateway-agent"
kind = "openai-compat"
endpoint = "http://127.0.0.1:18083"
model = "local-cloud-coding"
aliases = ["premium"]

[[proxy.providers]]
id = "remote"
backend_type = "http"
url = "https://example.invalid/v1"
models = ["premium"]
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "exact provider model IDs must not silently reuse agent aliases"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("premium") && e.contains("provider model ID") && e.contains("agent alias")
        }),
        "error should identify the provider model / agent alias collision; errors: {:?}",
        result.errors
    );
}

#[test]
fn model_shortcut_alias_cannot_shadow_agent_selector() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[agents]]
id = "gateway-agent"
kind = "openai-compat"
endpoint = "http://127.0.0.1:18083"
model = "local-cloud-coding"
aliases = ["premium"]

[[model_shortcuts]]
alias = "premium"
model = "openai/gpt-5.5"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "model shortcut aliases must not silently reuse agent selectors"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("premium") && e.contains("model shortcut alias") && e.contains("agent alias")
        }),
        "error should identify shortcut / agent alias collision; errors: {:?}",
        result.errors
    );
}

#[test]
fn exec_models_are_deprecated_config_errors() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[exec_models]]
id = "codex/gpt-5.5"
context_window = 262144
command = "codex"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "deprecated exec model shims should not silently register as gateway models"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("[[exec_models]]")
                && e.contains("deprecated")
                && e.contains("kind = \"exec\"")
        }),
        "error should explain the agent migration path; errors: {:?}",
        result.errors
    );
}

#[test]
fn exec_proxy_providers_are_deprecated_config_errors() {
    let fixture = format!(
        r#"
{MIN_VALID}

[proxy]
enabled = true
bind = "127.0.0.1:18083"

[[proxy.providers]]
id = "codex-cli"
backend_type = "exec"
models = ["codex/gpt-5.5"]
command = "codex"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "exec proxy providers must not silently register as gateway models"
    );
    assert!(
        result.errors.iter().any(|e| {
            e.contains("codex-cli")
                && e.contains("backend_type = \"exec\"")
                && e.contains("[[agents]]")
        }),
        "error should explain that CLI-backed subscriptions are agents; errors: {:?}",
        result.errors
    );
}

#[test]
fn duplicate_first_class_model_ids_are_config_errors() {
    let fixture = format!(
        r#"
{MIN_VALID}

[[dispatchers]]
id = "balanced"

[[dispatchers.models]]
model = "qwen-test:small"
context_window = 60000

[[proxy.model_routes]]
pattern = "balanced"
provider = "missing-provider"
"#
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        !result.is_valid(),
        "configured first-class model IDs must not collide across namespaces"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("Ambiguous model selector 'balanced'")),
        "error should identify duplicate first-class model ID; errors: {:?}",
        result.errors
    );
}

#[test]
fn whatsapp_legacy_webhook_fields_are_config_errors() {
    let fixture = r#"
[calciforge]
version = 2

[[channels]]
kind = "whatsapp"
enabled = true
whatsapp_session_path = "/tmp/calciforge-wa.db"
webhook_listen = "0.0.0.0:18795"
webhook_path = "/webhooks/whatsapp"
zeroclaw_endpoint = "http://127.0.0.1:18796"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "legacy webhook fields must fail before startup"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("WhatsApp")
            && e.contains("webhook_listen")
            && e.contains("embedded channel schema")),
        "error should explain the embedded-channel migration; errors: {:?}",
        result.errors
    );
}

#[test]
fn enabled_whatsapp_requires_session_path() {
    let fixture = r#"
[calciforge]
version = 2

[[channels]]
kind = "whatsapp"
enabled = true
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "enabled WhatsApp without session storage should fail"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("whatsapp_session_path")),
        "error should name whatsapp_session_path; errors: {:?}",
        result.errors
    );
}

#[test]
fn signal_legacy_webhook_fields_are_config_errors() {
    let fixture = r#"
[calciforge]
version = 2

[[channels]]
kind = "signal"
enabled = true
signal_cli_url = "http://127.0.0.1:8080"
signal_account = "+15555550001"
webhook_path = "/webhooks/signal"
"#;
    let config = parse(fixture);
    let result = validate_config(&config);
    assert!(
        !result.is_valid(),
        "legacy webhook fields must fail before startup"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("Signal")
            && e.contains("webhook_path")
            && e.contains("embedded channel schema")),
        "error should explain the embedded-channel migration; errors: {:?}",
        result.errors
    );
}
