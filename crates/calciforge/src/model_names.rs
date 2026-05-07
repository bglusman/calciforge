//! Shared model-name helpers.
//!
//! Model shortcut aliases are part of Calciforge's public configuration
//! surface. Resolve them in one place so chat commands, API requests, synthetic
//! expansion, and future control surfaces do not drift apart.

use std::collections::HashSet;

use crate::config::{CalciforgeConfig, ModelShortcutConfig};

/// Configured model ID kind used for namespace validation and diagnostics.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ModelIdKind {
    Synthetic,
    Local,
    ProviderExact,
    ModelRouteExact,
}

impl ModelIdKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic model ID",
            Self::Local => "local model ID",
            Self::ProviderExact => "provider model ID",
            Self::ModelRouteExact => "model-route ID",
        }
    }
}

/// A configured first-class model ID.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConfiguredModelId {
    pub id: String,
    pub kind: ModelIdKind,
}

/// Configured agent selector kind used for cross-namespace validation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AgentSelectorKind {
    AgentId,
    AgentAlias,
}

impl AgentSelectorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AgentId => "agent ID",
            Self::AgentAlias => "agent alias",
        }
    }
}

/// A configured public selector for an agent.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConfiguredAgentSelector {
    pub id: String,
    pub kind: AgentSelectorKind,
    pub owner_agent_id: String,
}

/// Return true when a provider/model-route pattern is a selectable exact model
/// ID rather than a wildcard route.
pub fn is_exact_model_pattern(pattern: &str) -> bool {
    let pattern = pattern.trim();
    !pattern.is_empty() && pattern != "*" && !pattern.contains('*')
}

/// Collect model IDs Calciforge treats as first-class configured names.
///
/// Shortcut aliases are intentionally excluded: callers validate aliases
/// against this namespace so alias resolution cannot silently shadow a real
/// configured model ID.
pub fn configured_first_class_model_ids(config: &CalciforgeConfig) -> Vec<ConfiguredModelId> {
    let mut ids = Vec::new();

    ids.extend(config.alloys.iter().map(|alloy| ConfiguredModelId {
        id: alloy.id.clone(),
        kind: ModelIdKind::Synthetic,
    }));
    ids.extend(config.cascades.iter().map(|cascade| ConfiguredModelId {
        id: cascade.id.clone(),
        kind: ModelIdKind::Synthetic,
    }));
    ids.extend(
        config
            .dispatchers
            .iter()
            .map(|dispatcher| ConfiguredModelId {
                id: dispatcher.id.clone(),
                kind: ModelIdKind::Synthetic,
            }),
    );
    if let Some(local_models) = &config.local_models {
        ids.extend(local_models.models.iter().map(|model| ConfiguredModelId {
            id: model.id.clone(),
            kind: ModelIdKind::Local,
        }));
    }

    if let Some(proxy) = &config.proxy {
        for provider in &proxy.providers {
            ids.extend(
                provider
                    .models
                    .iter()
                    .filter(|model| is_exact_model_pattern(model))
                    .map(|model| ConfiguredModelId {
                        id: model.clone(),
                        kind: ModelIdKind::ProviderExact,
                    }),
            );
        }

        ids.extend(
            proxy
                .model_routes
                .iter()
                .filter(|route| is_exact_model_pattern(&route.pattern))
                .map(|route| ConfiguredModelId {
                    id: route.pattern.clone(),
                    kind: ModelIdKind::ModelRouteExact,
                }),
        );
    }

    ids
}

/// Collect agent selectors users can type in routing commands.
pub fn configured_agent_selectors(config: &CalciforgeConfig) -> Vec<ConfiguredAgentSelector> {
    let mut selectors = Vec::new();

    for agent in &config.agents {
        selectors.push(ConfiguredAgentSelector {
            id: agent.id.clone(),
            kind: AgentSelectorKind::AgentId,
            owner_agent_id: agent.id.clone(),
        });
        selectors.extend(agent.aliases.iter().map(|alias| ConfiguredAgentSelector {
            id: alias.clone(),
            kind: AgentSelectorKind::AgentAlias,
            owner_agent_id: agent.id.clone(),
        }));
    }

    selectors
}

/// Resolve a shortcut alias chain to the first non-alias model name.
pub fn resolve_model_alias_chain(
    shortcuts: &[ModelShortcutConfig],
    model: &str,
) -> Result<String, String> {
    let mut current = model.to_string();
    let mut seen_order = Vec::new();
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current.clone()) {
            seen_order.push(current.clone());
            return Err(format!(
                "model shortcut cycle detected while resolving '{}': {}",
                model,
                seen_order.join(" -> ")
            ));
        }
        seen_order.push(current.clone());

        let Some(next) = shortcuts
            .iter()
            .find(|shortcut| shortcut.alias == current)
            .map(|shortcut| shortcut.model.clone())
        else {
            return Ok(current);
        };
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CalciforgeConfig, LocalModelDef, LocalModelsConfig, ProxyConfig, ProxyModelRoute,
        ProxyProviderConfig,
    };

    #[test]
    fn resolves_alias_chains() {
        let shortcuts = vec![
            ModelShortcutConfig {
                alias: "fast".to_string(),
                model: "local".to_string(),
            },
            ModelShortcutConfig {
                alias: "local".to_string(),
                model: "qwen3.6:27b".to_string(),
            },
        ];

        assert_eq!(
            resolve_model_alias_chain(&shortcuts, "fast").unwrap(),
            "qwen3.6:27b"
        );
    }

    #[test]
    fn rejects_alias_cycles() {
        let shortcuts = vec![
            ModelShortcutConfig {
                alias: "a".to_string(),
                model: "b".to_string(),
            },
            ModelShortcutConfig {
                alias: "b".to_string(),
                model: "a".to_string(),
            },
        ];

        let err = resolve_model_alias_chain(&shortcuts, "a").unwrap_err();

        assert!(err.contains("a -> b -> a"), "{err}");
    }

    #[test]
    fn exact_model_patterns_exclude_wildcards() {
        assert!(is_exact_model_pattern("qwen3.6:27b"));
        assert!(is_exact_model_pattern("openai/gpt-5.5"));
        assert!(!is_exact_model_pattern("*"));
        assert!(!is_exact_model_pattern("openai/*"));
        assert!(!is_exact_model_pattern("claude-*"));
    }

    #[test]
    fn configured_model_namespace_collects_first_class_ids() {
        let mut config: CalciforgeConfig = toml::from_str(
            r#"
[calciforge]
version = 2
"#,
        )
        .unwrap();
        config.local_models = Some(LocalModelsConfig {
            models: vec![LocalModelDef {
                id: "local-qwen".to_string(),
                hf_id: "example/qwen".to_string(),
                provider_type: "mlx_lm".to_string(),
                display_name: None,
            }],
            ..Default::default()
        });
        config.proxy = Some(ProxyConfig {
            providers: vec![ProxyProviderConfig {
                id: "remote".to_string(),
                backend_type: "http".to_string(),
                url: "https://example.invalid/v1".to_string(),
                api_key: None,
                api_key_file: None,
                models: vec![
                    "openai/gpt-5.5".to_string(),
                    "openai/*".to_string(),
                    "claude-*".to_string(),
                ],
                timeout_seconds: None,
                headers: Default::default(),
                on_switch: None,
                command: None,
                args: Vec::new(),
                env: Default::default(),
            }],
            model_routes: vec![ProxyModelRoute {
                pattern: "coding/default".to_string(),
                provider: "remote".to_string(),
            }],
            ..Default::default()
        });

        let ids = configured_first_class_model_ids(&config);
        let names: HashSet<_> = ids.iter().map(|entry| entry.id.as_str()).collect();

        assert!(names.contains("local-qwen"));
        assert!(names.contains("openai/gpt-5.5"));
        assert!(names.contains("coding/default"));
        assert!(!names.contains("openai/*"));
        assert!(!names.contains("claude-*"));
    }
}
