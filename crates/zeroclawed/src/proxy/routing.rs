//! Multi-provider model routing for the proxy.
//!
//! Builds a priority-ordered `Vec<ProviderEntry>` from `[[proxy.providers]]` and
//! `[[proxy.model_routes]]` config. The handler iterates entries in order and
//! uses the first match, falling back to the default gateway.

use std::collections::HashMap;

use anyhow::Context as _;
use tracing::info;

use crate::config::ProxyConfig;
use crate::sync::Arc;

use super::backend::{BackendConfig, BackendType};
use super::gateway::{self, GatewayBackend, GatewayConfig, GatewayType};

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
    /// Shell script to run on `!model <id>` switch to any model of this provider.
    pub on_switch: Option<String>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("id", &self.id)
            .field("patterns", &self.patterns)
            .field("on_switch", &self.on_switch)
            .finish()
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

    for p in &config.providers {
        // Resolve API key (file takes precedence).
        let api_key = if let Some(ref file) = p.api_key_file {
            let raw = std::fs::read_to_string(file)
                .with_context(|| format!("reading API key file for provider '{}'", p.id))?;
            raw.trim_end().to_string()
        } else {
            p.api_key.clone().unwrap_or_default()
        };

        let timeout = p.timeout_seconds.unwrap_or(default_timeout);
        let headers: Option<HashMap<String, String>> = if p.headers.is_empty() {
            None
        } else {
            Some(p.headers.clone())
        };

        let backend_cfg = BackendConfig {
            backend_type: BackendType::Http,
            url: Some(p.url.clone()),
            api_key: Some(api_key.clone()),
            timeout_seconds: Some(timeout),
            headers: headers.clone(),
            ..Default::default()
        };

        let backend = super::backend::create_backend(&backend_cfg)
            .with_context(|| format!("creating backend for provider '{}'", p.id))?;

        let gw_cfg = GatewayConfig {
            backend_type: GatewayType::Direct,
            base_url: Some(p.url.clone()),
            api_key: Some(api_key),
            timeout_seconds: timeout,
            extra_config: None,
            headers,
            retry_enabled: true,
            max_retries: 3,
            retry_base_delay_ms: 1000,
            retry_max_delay_ms: 10000,
        };

        let gw = gateway::create_gateway(gw_cfg, Some(backend))
            .with_context(|| format!("creating gateway for provider '{}'", p.id))?;

        info!(id = %p.id, url = %p.url, models = ?p.models, "Provider loaded");
        provider_gateways.insert(p.id.clone(), gw);
        provider_on_switch.insert(p.id.clone(), p.on_switch.clone());
    }

    let mut entries: Vec<ProviderEntry> = Vec::new();

    // 1. model_routes first (explicit overrides, highest priority).
    for route in &config.model_routes {
        if let Some(gw) = provider_gateways.get(&route.provider) {
            entries.push(ProviderEntry {
                id: format!("route:{}->{}", route.pattern, route.provider),
                patterns: vec![route.pattern.clone()],
                gateway: Arc::clone(gw),
                on_switch: provider_on_switch.get(&route.provider).cloned().flatten(),
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
            });
        }
    }

    Ok(entries)
}
