//! Shared model-name helpers.
//!
//! Model shortcut aliases are part of Calciforge's public configuration
//! surface. Resolve them in one place so chat commands, API requests, synthetic
//! expansion, and future control surfaces do not drift apart.

use std::collections::HashSet;

use crate::config::ModelShortcutConfig;

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
}
