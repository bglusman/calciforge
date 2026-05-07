//! Multi-provider model routing for the proxy.
//!
//! Builds a priority-ordered `Vec<ProviderEntry>` from explicit
//! `[[proxy.model_routes]]`, exec-backed model shims, and `[[proxy.providers]]`
//! config. The handler iterates entries in order and uses the first match,
//! falling back to the default gateway.

use std::collections::HashMap;

use anyhow::Context as _;
use tracing::info;

use crate::config::{ExecModelConfig, ProxyConfig};
use crate::sync::Arc;

use super::backend::{BackendConfig, BackendType};
use super::exec_gateway::ExecGateway;
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
/// 2. `[[exec_models]]` entries (exact model IDs backed by local commands)
/// 3. `[[proxy.providers]]` models patterns (in provider × pattern order)
pub fn build_provider_entries(
    config: &ProxyConfig,
    exec_models: &[ExecModelConfig],
    default_timeout: u64,
) -> anyhow::Result<Vec<ProviderEntry>> {
    // Build a map of provider_id → resolved gateway for efficient lookup.
    let mut provider_gateways: HashMap<String, Arc<dyn GatewayBackend>> = HashMap::new();
    let mut provider_on_switch: HashMap<String, Option<String>> = HashMap::new();

    for p in &config.providers {
        if p.backend_type == "exec" {
            let command = p
                .command
                .clone()
                .ok_or_else(|| anyhow::anyhow!("exec provider '{}' requires command", p.id))?;
            let timeout = p.timeout_seconds.unwrap_or(default_timeout);
            let gw_cfg = GatewayConfig {
                backend_type: GatewayType::Direct,
                base_url: None,
                api_key: None,
                timeout_seconds: timeout,
                extra_config: None,
                headers: None,
                retry_enabled: false,
                max_retries: 0,
                retry_base_delay_ms: 0,
                retry_max_delay_ms: 0,
                ui_url: None,
            };
            let gw = Arc::new(ExecGateway::new(
                gw_cfg,
                command,
                p.args.clone(),
                p.env.clone(),
            ));
            info!(id = %p.id, models = ?p.models, "Exec provider loaded");
            provider_gateways.insert(p.id.clone(), gw);
            provider_on_switch.insert(p.id.clone(), p.on_switch.clone());
            continue;
        }

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
                retry_enabled: true,
                max_retries: 3,
                retry_base_delay_ms: 1000,
                retry_max_delay_ms: 10000,
                ui_url: None,
            };
            let gw = gateway::create_gateway(gw_cfg, None)
                .with_context(|| format!("creating Helicone gateway for provider '{}'", p.id))?;
            info!(id = %p.id, url = %p.url, models = ?p.models, "Helicone provider loaded");
            provider_gateways.insert(p.id.clone(), gw);
            provider_on_switch.insert(p.id.clone(), p.on_switch.clone());
            continue;
        }

        if p.backend_type != "http" {
            anyhow::bail!(
                "provider '{}' has unsupported backend_type '{}'",
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
            backend_type: GatewayType::Direct,
            base_url: Some(p.url.clone()),
            api_key,
            timeout_seconds: timeout,
            extra_config: None,
            headers,
            retry_enabled: true,
            max_retries: 3,
            retry_base_delay_ms: 1000,
            retry_max_delay_ms: 10000,
            ui_url: None,
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

    // 2. Exec-backed model shims. These are exact terminal gateway selectors.
    for model in exec_models {
        let timeout = model.timeout_seconds.unwrap_or(default_timeout);
        let gw_cfg = GatewayConfig {
            backend_type: GatewayType::Direct,
            base_url: None,
            api_key: None,
            timeout_seconds: timeout,
            extra_config: None,
            headers: None,
            retry_enabled: false,
            max_retries: 0,
            retry_base_delay_ms: 0,
            retry_max_delay_ms: 0,
            ui_url: None,
        };
        let gw = Arc::new(ExecGateway::new(
            gw_cfg,
            model.command.clone(),
            model.args.clone(),
            model.env.clone(),
        ));
        info!(id = %model.id, "Exec-backed model shim loaded");
        entries.push(ProviderEntry {
            id: format!("exec:{}", model.id),
            patterns: vec![model.id.clone()],
            gateway: gw,
            on_switch: None,
        });
    }

    // 3. Provider model patterns (in declaration order).
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
    use crate::config::ProxyProviderConfig;

    fn provider(id: &str, backend_type: &str, url: &str) -> ProxyProviderConfig {
        ProxyProviderConfig {
            id: id.to_string(),
            backend_type: backend_type.to_string(),
            url: url.to_string(),
            api_key: None,
            api_key_file: None,
            models: vec!["test-model".to_string()],
            timeout_seconds: None,
            headers: HashMap::new(),
            on_switch: None,
            command: if backend_type == "exec" {
                Some("/bin/echo".to_string())
            } else {
                None
            },
            args: Vec::new(),
            env: HashMap::new(),
        }
    }

    #[test]
    fn http_provider_requires_non_empty_url() {
        let config = ProxyConfig {
            providers: vec![provider("missing-url", "http", "  ")],
            ..Default::default()
        };

        let err = build_provider_entries(&config, &[], 30).unwrap_err();
        assert!(err.to_string().contains("requires non-empty url"));
    }

    #[test]
    fn exec_provider_may_omit_url() {
        let config = ProxyConfig {
            providers: vec![provider("exec-provider", "exec", "")],
            ..Default::default()
        };

        let entries = build_provider_entries(&config, &[], 30).unwrap();
        assert_eq!(entries.len(), 1);
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

        let entries = build_provider_entries(&config, &[], 30).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].patterns, vec!["test-model"]);
        assert_eq!(entries[0].gateway.gateway_type(), GatewayType::Helicone);
    }

    #[test]
    fn exec_models_become_exact_provider_entries() {
        let config = ProxyConfig::default();
        let entries = build_provider_entries(
            &config,
            &[ExecModelConfig {
                id: "codex/gpt-5.5".to_string(),
                name: None,
                context_window: 262_144,
                command: "/bin/echo".to_string(),
                args: vec!["ok".to_string()],
                env: HashMap::new(),
                timeout_seconds: Some(10),
            }],
            30,
        )
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].patterns, vec!["codex/gpt-5.5"]);
        assert!(find_provider(&entries, "codex/gpt-5.5").is_some());
    }
}
