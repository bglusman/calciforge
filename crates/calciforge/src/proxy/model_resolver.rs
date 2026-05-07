//! Model identifier resolution for the gateway.
//!
//! The gateway accepts model names from several surfaces: API requests,
//! `[[model_shortcuts]]`, and synthetic model definitions. Keep shortcut
//! normalization and synthetic expansion here so auth, validation, and routing
//! do not each grow subtly different model-name rules.

use crate::config::ModelShortcutConfig;
use crate::model_names::resolve_model_alias_chain;
use crate::providers::alloy::{AlloyManager, AlloyPlan};

/// Resolves model shortcuts and synthetic model plans into concrete gateway
/// model names.
pub struct ModelResolver<'a> {
    shortcuts: &'a [ModelShortcutConfig],
    alloy_manager: &'a AlloyManager,
}

impl<'a> ModelResolver<'a> {
    pub fn new(shortcuts: &'a [ModelShortcutConfig], alloy_manager: &'a AlloyManager) -> Self {
        Self {
            shortcuts,
            alloy_manager,
        }
    }

    /// Resolve a shortcut alias chain to the first non-alias model name.
    ///
    /// This intentionally does not expand synthetic models. A shortcut may
    /// target a synthetic model ID, and callers can then ask for a plan.
    pub fn resolve_alias_chain(&self, model: &str) -> Result<String, String> {
        resolve_model_alias_chain(self.shortcuts, model)
    }

    /// Build the full route plan for a requested model name.
    ///
    /// The returned plan contains only canonical concrete model names in
    /// `ordered_models`; shortcut aliases and nested synthetic IDs are fully
    /// expanded before provider routing.
    pub fn plan_for_model(
        &self,
        model: &str,
        estimated_tokens: u32,
    ) -> Result<ResolvedModelPlan, String> {
        let root_model = self.resolve_alias_chain(model)?;
        let plan = match self
            .alloy_manager
            .select_plan_for_model(&root_model, estimated_tokens)?
        {
            Some(plan) => plan,
            None => AlloyPlan {
                alloy_id: root_model.clone(),
                alloy_name: root_model.clone(),
                ordered_models: vec![root_model.clone()],
            },
        };
        let plan = self.expand_plan(plan, estimated_tokens, &mut Vec::new())?;

        Ok(ResolvedModelPlan { root_model, plan })
    }

    fn expand_plan(
        &self,
        mut plan: AlloyPlan,
        estimated_tokens: u32,
        stack: &mut Vec<String>,
    ) -> Result<AlloyPlan, String> {
        if stack.iter().any(|id| id == &plan.alloy_id) {
            stack.push(plan.alloy_id.clone());
            return Err(format!(
                "synthetic model cycle detected after shortcut resolution: {}",
                stack.join(" -> ")
            ));
        }

        stack.push(plan.alloy_id.clone());
        let mut expanded = Vec::new();
        for target in &plan.ordered_models {
            let resolved = self.resolve_alias_chain(target)?;
            if resolved == plan.alloy_id && self.alloy_manager.is_exec_model(&resolved) {
                expanded.push(resolved);
                continue;
            }
            match self
                .alloy_manager
                .select_plan_for_model(&resolved, estimated_tokens)?
            {
                Some(child) => {
                    let child = self.expand_plan(child, estimated_tokens, stack)?;
                    expanded.extend(child.ordered_models);
                }
                None => expanded.push(resolved),
            }
        }
        stack.pop();

        plan.ordered_models = expanded;
        Ok(plan)
    }
}

pub struct ResolvedModelPlan {
    /// Request model after resolving shortcut aliases, before synthetic
    /// expansion. This may still be a synthetic model ID.
    pub root_model: String,
    /// Fully expanded route plan. `ordered_models` are concrete gateway models.
    pub plan: AlloyPlan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DispatcherConfig, ExecModelConfig, SyntheticModelConfig};

    fn dispatcher_manager() -> AlloyManager {
        AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[
                DispatcherConfig {
                    id: "outer".to_string(),
                    name: None,
                    models: vec![SyntheticModelConfig {
                        model: "inner-alias".to_string(),
                        context_window: 250_000,
                    }],
                },
                DispatcherConfig {
                    id: "inner".to_string(),
                    name: None,
                    models: vec![SyntheticModelConfig {
                        model: "local".to_string(),
                        context_window: 60_000,
                    }],
                },
            ],
            &[],
        )
        .unwrap()
    }

    #[test]
    fn resolves_aliases_inside_nested_synthetic_plans() {
        let manager = dispatcher_manager();
        let shortcuts = vec![
            ModelShortcutConfig {
                alias: "inner-alias".to_string(),
                model: "inner".to_string(),
            },
            ModelShortcutConfig {
                alias: "local".to_string(),
                model: "qwen3.6:27b".to_string(),
            },
        ];
        let resolver = ModelResolver::new(&shortcuts, &manager);

        let resolved = resolver.plan_for_model("outer", 1_000).unwrap();

        assert_eq!(resolved.root_model, "outer");
        assert_eq!(resolved.plan.ordered_models, vec!["qwen3.6:27b"]);
    }

    #[test]
    fn rejects_shortcut_cycles() {
        let manager = AlloyManager::empty();
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
        let resolver = ModelResolver::new(&shortcuts, &manager);

        let err = resolver.resolve_alias_chain("a").unwrap_err();

        assert!(err.contains("shortcut cycle"), "{err}");
    }

    #[test]
    fn preserves_exec_models_as_terminal_synthetic_leaves() {
        let manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[],
            &[ExecModelConfig {
                id: "exec/fake".to_string(),
                name: Some("Fake exec".to_string()),
                context_window: 4_000,
                command: "/bin/echo".to_string(),
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                timeout_seconds: None,
            }],
        )
        .unwrap();
        let resolver = ModelResolver::new(&[], &manager);

        let resolved = resolver.plan_for_model("exec/fake", 100).unwrap();

        assert_eq!(resolved.root_model, "exec/fake");
        assert_eq!(resolved.plan.ordered_models, vec!["exec/fake"]);
    }
}
