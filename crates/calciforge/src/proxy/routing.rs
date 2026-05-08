//! Multi-provider model routing for the proxy.
//!
//! Builds a priority-ordered `Vec<ProviderEntry>` from explicit
//! `[[proxy.model_routes]]` and `[[proxy.providers]]` config. The handler
//! iterates entries in order and uses the first match, falling back to the
//! default gateway.

use std::collections::HashMap;

use anyhow::Context as _;
use tracing::info;

use crate::config::{GatewayFailureKind, ProxyConfig};
use crate::sync::Arc;

use super::backend::{BackendConfig, BackendType};
use super::gateway::{self, GatewayBackend, GatewayConfig, GatewayType};
use super::openai::is_reserved_chat_completion_field;

/// Per-provider model switch state shared by routes that point to the same provider.
#[derive(Debug, Default)]
pub struct ProviderSwitchState {
    current_model: tokio::sync::Mutex<Option<String>>,
}

impl ProviderSwitchState {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, Option<String>> {
        self.current_model.lock().await
    }
}

/// A resolved provider entry: a set of model-name patterns and a ready gateway.
#[derive(Clone)]
pub struct ProviderEntry {
    /// Provider ID from config (for logging).
    pub id: String,
    /// Model name patterns this entry handles, in declaration order.
    /// Supports exact match and `prefix/*` glob.
    pub patterns: Vec<String>,
    /// Gateway to use for matching requests.
    pub gateway: Arc<dyn GatewayBackend>,
    /// Shell script to run before a gateway request switches to any model of this provider.
    pub on_switch: Option<String>,
    /// Shared state for serializing provider model swaps before gateway requests.
    pub switch_state: Arc<ProviderSwitchState>,
    /// Optional public model prefix stripped before forwarding upstream.
    pub strip_model_prefix: Option<String>,
    /// Optional provider model prefix added before forwarding upstream.
    pub add_model_prefix: Option<String>,
    /// Failure classes that may advance synthetic fallback from this provider.
    pub fallback_on: Vec<GatewayFailureKind>,
    /// Provider-specific OpenAI-compatible extension fields to merge into the
    /// upstream request body.
    pub request_body: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("id", &self.id)
            .field("patterns", &self.patterns)
            .field("on_switch", &self.on_switch)
            .field("strip_model_prefix", &self.strip_model_prefix)
            .field("add_model_prefix", &self.add_model_prefix)
            .field("fallback_on", &self.fallback_on)
            .field(
                "request_body_keys",
                &self.request_body.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ProviderEntry {
    pub fn upstream_model_name(&self, model: &str) -> String {
        let mut upstream = if let Some(prefix) = self
            .strip_model_prefix
            .as_deref()
            .filter(|prefix| !prefix.is_empty())
        {
            if let Some(stripped) = model.strip_prefix(prefix) {
                stripped.to_string()
            } else {
                model.to_string()
            }
        } else {
            model.to_string()
        };

        if let Some(prefix) = self
            .add_model_prefix
            .as_deref()
            .filter(|prefix| !prefix.is_empty())
        {
            if !upstream.starts_with(prefix) {
                upstream = format!("{prefix}{upstream}");
            }
        }

        upstream
    }
}

/// Check whether a model name matches a pattern.
/// Supports exact match and `prefix/*` glob (e.g. `kimi/*` matches `kimi/kimi-for-coding`).
pub fn model_matches_pattern(model: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return model.starts_with(prefix);
    }
    model == pattern
}

/// Find the first provider entry whose patterns match the given model name.
pub fn find_provider<'a>(providers: &'a [ProviderEntry], model: &str) -> Option<&'a ProviderEntry> {
    providers
        .iter()
        .find(|e| e.patterns.iter().any(|p| model_matches_pattern(model, p)))
}

/// Build the ordered `Vec<ProviderEntry>` from proxy config.
///
/// Priority order:
/// 1. `[[proxy.model_routes]]` entries (explicit overrides, in declaration order)
/// 2. `[[proxy.providers]]` models patterns (in provider × pattern order)
pub fn build_provider_entries(
    config: &ProxyConfig,
    default_timeout: u64,
) -> anyhow::Result<Vec<ProviderEntry>> {
    // Build a map of provider_id → resolved gateway for efficient lookup.
    let mut provider_gateways: HashMap<String, Arc<dyn GatewayBackend>> = HashMap::new();
    let mut provider_on_switch: HashMap<String, Option<String>> = HashMap::new();
    let mut provider_strip_prefix: HashMap<String, Option<String>> = HashMap::new();
    let mut provider_add_prefix: HashMap<String, Option<String>> = HashMap::new();
    let mut provider_request_body: HashMap<String, serde_json::Map<String, serde_json::Value>> =
        HashMap::new();
    let mut provider_switch_state: HashMap<String, Arc<ProviderSwitchState>> = HashMap::new();

    for p in &config.providers {
        validate_request_body_extensions(p)?;
        provider_switch_state
            .entry(p.id.clone())
            .or_insert_with(|| Arc::new(ProviderSwitchState::default()));
        if p.backend_type == "helicone" {
            if p.url.trim().is_empty() {
                anyhow::bail!(
                    "provider '{}' with backend_type 'helicone' requires non-empty url",
                    p.id
                );
            }
            let api_key = resolve_provider_api_key(p)?;
            let timeout = p.timeout_seconds.unwrap_or(default_timeout);
            let headers: Option<HashMap<String, String>> = if p.headers.is_empty() {
                None
            } else {
                Some(p.headers.clone())
            };
            let gw_cfg = GatewayConfig {
                backend_type: GatewayType::Helicone,
                base_url: Some(p.url.clone()),
                api_key,
                timeout_seconds: timeout,
                extra_config: None,
                headers,
                retry: p.retry.clone().unwrap_or_else(|| config.retry.clone()),
                ui_url: None,
            };
            let gw = gateway::create_gateway(gw_cfg, None)
                .with_context(|| format!("creating Helicone gateway for provider '{}'", p.id))?;
            info!(id = %p.id, url = %p.url, models = ?p.models, "Helicone provider loaded");
            provider_gateways.insert(p.id.clone(), gw);
            provider_on_switch.insert(p.id.clone(), p.on_switch.clone());
            provider_strip_prefix.insert(p.id.clone(), normalized_strip_prefix(p));
            provider_add_prefix.insert(p.id.clone(), normalized_add_prefix(p));
            provider_request_body.insert(p.id.clone(), request_body_map(p));
            continue;
        }

        if p.backend_type != "http" {
            anyhow::bail!(
                "provider '{}' has unsupported backend_type '{}'; use 'http' or 'helicone'. CLI-backed subscriptions must be configured as [[agents]], not gateway providers.",
                p.id,
                p.backend_type
            );
        }
        if p.url.trim().is_empty() {
            anyhow::bail!(
                "provider '{}' with backend_type 'http' requires non-empty url",
                p.id
            );
        }

        let api_key = resolve_provider_api_key(p)?;

        let timeout = p.timeout_seconds.unwrap_or(default_timeout);
        let headers: Option<HashMap<String, String>> = if p.headers.is_empty() {
            None
        } else {
            Some(p.headers.clone())
        };

        let backend_cfg = BackendConfig {
            backend_type: BackendType::Http,
            url: Some(p.url.clone()),
            api_key: api_key.clone(),
            timeout_seconds: Some(timeout),
            headers: headers.clone(),
            ..Default::default()
        };

        let backend = super::backend::create_backend(&backend_cfg)
            .with_context(|| format!("creating backend for provider '{}'", p.id))?;

        let gw_cfg = GatewayConfig {
            backend_type: GatewayType::BuiltinHttp,
            base_url: Some(p.url.clone()),
            api_key,
            timeout_seconds: timeout,
            extra_config: None,
            headers,
            retry: p.retry.clone().unwrap_or_else(|| config.retry.clone()),
            ui_url: None,
        };

        let gw = gateway::create_gateway(gw_cfg, Some(backend))
            .with_context(|| format!("creating gateway for provider '{}'", p.id))?;

        info!(id = %p.id, url = %p.url, models = ?p.models, "Provider loaded");
        provider_gateways.insert(p.id.clone(), gw);
        provider_on_switch.insert(p.id.clone(), p.on_switch.clone());
        provider_strip_prefix.insert(p.id.clone(), normalized_strip_prefix(p));
        provider_add_prefix.insert(p.id.clone(), normalized_add_prefix(p));
        provider_request_body.insert(p.id.clone(), request_body_map(p));
    }

    let mut entries: Vec<ProviderEntry> = Vec::new();

    // 1. model_routes first (explicit overrides, highest priority).
    for route in &config.model_routes {
        if let Some(gw) = provider_gateways.get(&route.provider) {
            entries.push(ProviderEntry {
                id: route.provider.clone(),
                patterns: vec![route.pattern.clone()],
                gateway: Arc::clone(gw),
                on_switch: provider_on_switch.get(&route.provider).cloned().flatten(),
                switch_state: provider_switch_state
                    .get(&route.provider)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(ProviderSwitchState::default())),
                strip_model_prefix: provider_strip_prefix
                    .get(&route.provider)
                    .cloned()
                    .flatten(),
                add_model_prefix: provider_add_prefix.get(&route.provider).cloned().flatten(),
                fallback_on: config
                    .providers
                    .iter()
                    .find(|p| p.id == route.provider)
                    .map(|p| provider_fallback_on(config, p))
                    .unwrap_or_else(|| config.fallback_on.clone()),
                request_body: provider_request_body
                    .get(&route.provider)
                    .cloned()
                    .unwrap_or_default(),
            });
        } else {
            anyhow::bail!(
                "model_route pattern '{}' references unknown provider '{}'",
                route.pattern,
                route.provider
            );
        }
    }

    // 2. Provider model patterns (in declaration order).
    for p in &config.providers {
        if p.models.is_empty() {
            continue;
        }
        if let Some(gw) = provider_gateways.get(&p.id) {
            entries.push(ProviderEntry {
                id: p.id.clone(),
                patterns: p.models.clone(),
                gateway: Arc::clone(gw),
                on_switch: provider_on_switch.get(&p.id).cloned().flatten(),
                switch_state: provider_switch_state
                    .get(&p.id)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(ProviderSwitchState::default())),
                strip_model_prefix: provider_strip_prefix.get(&p.id).cloned().flatten(),
                add_model_prefix: provider_add_prefix.get(&p.id).cloned().flatten(),
                fallback_on: provider_fallback_on(config, p),
                request_body: provider_request_body
                    .get(&p.id)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
    }

    Ok(entries)
}

fn provider_fallback_on(
    config: &ProxyConfig,
    provider: &crate::config::ProxyProviderConfig,
) -> Vec<GatewayFailureKind> {
    provider
        .fallback_on
        .clone()
        .unwrap_or_else(|| config.fallback_on.clone())
}

fn normalized_strip_prefix(provider: &crate::config::ProxyProviderConfig) -> Option<String> {
    provider
        .strip_model_prefix
        .as_deref()
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
}

fn normalized_add_prefix(provider: &crate::config::ProxyProviderConfig) -> Option<String> {
    provider
        .add_model_prefix
        .as_deref()
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
}

fn request_body_map(
    provider: &crate::config::ProxyProviderConfig,
) -> serde_json::Map<String, serde_json::Value> {
    provider
        .request_body
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn validate_request_body_extensions(
    provider: &crate::config::ProxyProviderConfig,
) -> anyhow::Result<()> {
    let mut reserved: Vec<&str> = provider
        .request_body
        .keys()
        .map(String::as_str)
        .filter(|key| is_reserved_chat_completion_field(key))
        .collect();
    reserved.sort_unstable();

    if !reserved.is_empty() {
        anyhow::bail!(
            "provider '{}' request_body may only contain provider extension fields; reserved OpenAI fields are not allowed: {}",
            provider.id,
            reserved.join(", ")
        );
    }

    Ok(())
}

fn resolve_provider_api_key(
    provider: &crate::config::ProxyProviderConfig,
) -> anyhow::Result<Option<String>> {
    let api_key = if let Some(ref file) = provider.api_key_file {
        let raw = std::fs::read_to_string(file)
            .with_context(|| format!("reading API key file for provider '{}'", provider.id))?;
        raw.trim().to_string()
    } else {
        provider
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .unwrap_or_default()
            .to_string()
    };
    Ok(if api_key.is_empty() {
        None
    } else {
        Some(api_key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProxyModelRoute, ProxyProviderConfig};

    fn provider(id: &str, backend_type: &str, url: &str) -> ProxyProviderConfig {
        ProxyProviderConfig {
            id: id.to_string(),
            backend_type: backend_type.to_string(),
            url: url.to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["test-model".to_string()],
            strip_model_prefix: None,
            add_model_prefix: None,
            timeout_seconds: None,
            headers: HashMap::new(),
            on_switch: None,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn http_provider_requires_non_empty_url() {
        let config = ProxyConfig {
            providers: vec![provider("missing-url", "http", "  ")],
            ..Default::default()
        };

        let err = build_provider_entries(&config, 30).unwrap_err();
        assert!(err.to_string().contains("requires non-empty url"));
    }

    #[test]
    fn exec_provider_is_rejected_because_cli_subscriptions_are_agents() {
        let config = ProxyConfig {
            providers: vec![provider("exec-provider", "exec", "")],
            ..Default::default()
        };

        let err = build_provider_entries(&config, 30).unwrap_err();
        assert!(
            err.to_string().contains("configured as [[agents]]"),
            "{err}"
        );
    }

    #[test]
    fn upstream_model_name_can_strip_public_prefix_and_add_provider_prefix() {
        let config = ProxyConfig {
            providers: vec![ProxyProviderConfig {
                id: "helicone-ollama".to_string(),
                backend_type: "helicone".to_string(),
                url: "http://127.0.0.1:8787/ai".to_string(),
                api_key: None,
                api_key_file: None,
                models: vec!["local/qwen3.6:27b".to_string()],
                strip_model_prefix: Some("local/".to_string()),
                add_model_prefix: Some("ollama/".to_string()),
                timeout_seconds: None,
                headers: HashMap::new(),
                on_switch: None,
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let entries = build_provider_entries(&config, 30).unwrap();

        assert_eq!(
            entries[0].upstream_model_name("local/qwen3.6:27b"),
            "ollama/qwen3.6:27b"
        );
        assert_eq!(
            entries[0].upstream_model_name("ollama/qwen3.6:27b"),
            "ollama/qwen3.6:27b",
            "already-qualified provider model IDs should not get double-prefixed"
        );
    }

    #[test]
    fn provider_request_body_rejects_reserved_openai_fields() {
        let mut p = provider("bad-request-body", "http", "https://example.invalid/v1");
        p.request_body
            .insert("model".to_string(), serde_json::json!("other-model"));
        p.request_body.insert(
            "thinking".to_string(),
            serde_json::json!({ "type": "disabled" }),
        );
        let config = ProxyConfig {
            providers: vec![p],
            ..Default::default()
        };

        let err = build_provider_entries(&config, 30).unwrap_err();
        assert!(err.to_string().contains("reserved OpenAI fields"), "{err}");
        assert!(err.to_string().contains("model"), "{err}");
        assert!(
            !err.to_string().contains("thinking"),
            "provider extension fields should remain allowed: {err}"
        );
    }

    #[test]
    fn model_route_entry_preserves_real_provider_id_for_hooks() {
        let config = ProxyConfig {
            providers: vec![ProxyProviderConfig {
                id: "helicone-ollama".to_string(),
                backend_type: "helicone".to_string(),
                url: "http://127.0.0.1:8787/ai".to_string(),
                api_key: None,
                api_key_file: None,
                models: vec![],
                strip_model_prefix: Some("local/".to_string()),
                add_model_prefix: Some("ollama/".to_string()),
                timeout_seconds: None,
                headers: HashMap::new(),
                on_switch: Some("/usr/local/bin/switch-model".to_string()),
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                ..Default::default()
            }],
            model_routes: vec![ProxyModelRoute {
                pattern: "local/qwen3.6:27b".to_string(),
                provider: "helicone-ollama".to_string(),
            }],
            ..Default::default()
        };

        let entries = build_provider_entries(&config, 30).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].id, "helicone-ollama",
            "model_routes must preserve the real provider id for hook env/log keys"
        );
        assert_eq!(entries[0].patterns, vec!["local/qwen3.6:27b"]);
        assert_eq!(
            entries[0].on_switch.as_deref(),
            Some("/usr/local/bin/switch-model")
        );
        assert_eq!(
            entries[0].upstream_model_name("local/qwen3.6:27b"),
            "ollama/qwen3.6:27b"
        );
    }

    #[cfg(feature = "helicone")]
    #[test]
    fn helicone_provider_uses_helicone_gateway_auth_path() {
        let config = ProxyConfig {
            providers: vec![provider(
                "helicone-local",
                "helicone",
                "http://127.0.0.1:8787/ollama/v1",
            )],
            ..Default::default()
        };

        let entries = build_provider_entries(&config, 30).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].patterns, vec!["test-model"]);
        assert_eq!(entries[0].gateway.gateway_type(), GatewayType::Helicone);
    }
}
