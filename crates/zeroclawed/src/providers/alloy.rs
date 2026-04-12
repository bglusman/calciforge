//! Alloy provider runtime selection and analytics.
//!
//! This module implements weighted/round-robin constituent selection,
//! fallback ordering for graceful degradation, and per-constituent usage stats.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use rand::distr::Distribution;
use rand::distr::weighted::WeightedIndex;
use rand::rng;
use serde::Serialize;

use crate::config::{AlloyConfig, AlloyConstituentConfig};

/// Runtime strategy for constituent selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlloyStrategy {
    /// Weighted random (based on configured constituent weights).
    Weighted,
    /// Deterministic round-robin rotation.
    RoundRobin,
}

impl AlloyStrategy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "weighted" => Some(Self::Weighted),
            "round_robin" => Some(Self::RoundRobin),
            _ => None,
        }
    }
}

/// One constituent with validated config.
#[derive(Debug, Clone)]
pub struct AlloyConstituent {
    pub model: String,
    pub weight: u32,
}

/// Immutable alloy definition.
#[derive(Debug, Clone)]
pub struct AlloyDefinition {
    pub id: String,
    pub name: String,
    pub strategy: AlloyStrategy,
    pub constituents: Vec<AlloyConstituent>,
}

/// Per-constituent aggregate counters.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConstituentStats {
    pub model: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
}

impl ConstituentStats {
    pub fn success_rate(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.successes as f64 / self.requests as f64
        }
    }
}

#[derive(Debug, Default, Clone)]
struct AlloyState {
    next_rr_index: usize,
    stats_by_model: HashMap<String, ConstituentStats>,
    last_selected_model: Option<String>,
}

/// A single selected alloy attempt plan.
#[derive(Debug, Clone)]
pub struct AlloyPlan {
    pub alloy_id: String,
    pub alloy_name: String,
    /// Ordered constituent models to try (first is primary selection).
    pub ordered_models: Vec<String>,
}

/// Runtime alloy provider object.
#[derive(Debug)]
pub struct AlloyProvider {
    def: AlloyDefinition,
    state: Mutex<AlloyState>,
}

impl Clone for AlloyProvider {
    fn clone(&self) -> Self {
        Self {
            def: self.def.clone(),
            state: Mutex::new(self.state.lock().expect("mutex poisoned").clone()),
        }
    }
}

impl AlloyProvider {
    /// Build a provider from config with validation.
    pub fn from_config(cfg: &AlloyConfig) -> Result<Self, String> {
        let strategy = AlloyStrategy::parse(&cfg.strategy)
            .ok_or_else(|| format!("alloy '{}': invalid strategy '{}'", cfg.id, cfg.strategy))?;

        if cfg.constituents.len() < 2 {
            return Err(format!(
                "alloy '{}': must define at least 2 constituents",
                cfg.id
            ));
        }

        let mut constituents = Vec::with_capacity(cfg.constituents.len());
        for c in &cfg.constituents {
            validate_constituent(cfg, c)?;
            constituents.push(AlloyConstituent {
                model: c.model.clone(),
                weight: c.weight,
            });
        }

        let mut stats_by_model = HashMap::new();
        for c in &constituents {
            stats_by_model.insert(
                c.model.clone(),
                ConstituentStats {
                    model: c.model.clone(),
                    ..Default::default()
                },
            );
        }

        Ok(Self {
            def: AlloyDefinition {
                id: cfg.id.clone(),
                name: cfg.name.clone(),
                strategy,
                constituents,
            },
            state: Mutex::new(AlloyState {
                next_rr_index: 0,
                stats_by_model,
                last_selected_model: None,
            }),
        })
    }

    /// Select an ordered list of constituents to try for this request.
    ///
    /// The first model is the primary selection. Remaining models are fallback
    /// candidates in deterministic order for graceful degradation.
    pub fn select_plan(&self) -> AlloyPlan {
        let mut state = self.state.lock().expect("alloy mutex poisoned");

        let ordered_models = match self.def.strategy {
            AlloyStrategy::RoundRobin => self.round_robin_order(&mut state),
            AlloyStrategy::Weighted => self.weighted_order(),
        };

        state.last_selected_model = ordered_models.first().cloned();

        AlloyPlan {
            alloy_id: self.def.id.clone(),
            alloy_name: self.def.name.clone(),
            ordered_models,
        }
    }

    fn round_robin_order(&self, state: &mut AlloyState) -> Vec<String> {
        let n = self.def.constituents.len();
        let start = state.next_rr_index % n;
        state.next_rr_index = (start + 1) % n;

        (0..n)
            .map(|offset| {
                let idx = (start + offset) % n;
                self.def.constituents[idx].model.clone()
            })
            .collect()
    }

    fn weighted_order(&self) -> Vec<String> {
        let mut remaining: Vec<AlloyConstituent> = self.def.constituents.clone();
        let mut ordered = Vec::with_capacity(remaining.len());
        let mut rng = rng();

        while !remaining.is_empty() {
            let weights: Vec<u32> = remaining
                .iter()
                .map(|c| if c.weight == 0 { 1 } else { c.weight })
                .collect();
            let idx: usize = match WeightedIndex::new(&weights) {
                Ok(dist) => dist.sample(&mut rng),
                Err(_) => 0,
            };
            let selected = remaining.remove(idx);
            ordered.push(selected.model);
        }

        ordered
    }

    /// Record one model attempt result.
    pub fn record_attempt(&self, model: &str, success: bool) {
        let mut state = self.state.lock().expect("alloy mutex poisoned");
        let entry = state
            .stats_by_model
            .entry(model.to_string())
            .or_insert_with(|| ConstituentStats {
                model: model.to_string(),
                ..Default::default()
            });
        entry.requests += 1;
        if success {
            entry.successes += 1;
        } else {
            entry.failures += 1;
        }
    }

    pub fn stats_snapshot(&self) -> Vec<ConstituentStats> {
        let state = self.state.lock().expect("alloy mutex poisoned");
        let mut out: Vec<ConstituentStats> = self
            .def
            .constituents
            .iter()
            .map(|c| {
                state
                    .stats_by_model
                    .get(&c.model)
                    .cloned()
                    .unwrap_or_else(|| ConstituentStats {
                        model: c.model.clone(),
                        ..Default::default()
                    })
            })
            .collect();
        out.sort_by(|a, b| a.model.cmp(&b.model));
        out
    }

    pub fn last_selected_model(&self) -> Option<String> {
        self.state
            .lock()
            .expect("alloy mutex poisoned")
            .last_selected_model
            .clone()
    }

    pub fn definition(&self) -> &AlloyDefinition {
        &self.def
    }
}

fn validate_constituent(alloy: &AlloyConfig, c: &AlloyConstituentConfig) -> Result<(), String> {
    if c.model.trim().is_empty() {
        return Err(format!(
            "alloy '{}': constituent model must not be empty",
            alloy.id
        ));
    }
    if c.weight == 0 {
        return Err(format!(
            "alloy '{}': constituent '{}' weight must be > 0",
            alloy.id, c.model
        ));
    }
    Ok(())
}

/// Manager for configured alloys and per-identity active alloy selection.
#[derive(Debug, Default)]
pub struct AlloyManager {
    alloys: HashMap<String, AlloyProvider>,
    active_by_identity: Mutex<HashMap<String, String>>,
}

impl Clone for AlloyManager {
    fn clone(&self) -> Self {
        Self {
            alloys: self.alloys.clone(),
            active_by_identity: Mutex::new(
                self.active_by_identity.lock().expect("mutex poisoned").clone()
            ),
        }
    }
}

impl AlloyManager {
    /// Create an empty alloy manager with no configured alloys.
    pub fn empty() -> Self {
        Self {
            alloys: HashMap::new(),
            active_by_identity: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_configs(configs: &[AlloyConfig]) -> Result<Self, String> {
        let mut alloys = HashMap::new();
        for cfg in configs {
            let provider = AlloyProvider::from_config(cfg)?;
            if alloys.insert(cfg.id.clone(), provider).is_some() {
                return Err(format!("duplicate alloy id '{}'", cfg.id));
            }
        }
        Ok(Self {
            alloys,
            active_by_identity: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.alloys.is_empty()
    }

    /// Get an alloy by ID.
    pub fn get_alloy(&self, alloy_id: &str) -> Option<&AlloyProvider> {
        self.alloys.get(alloy_id)
    }

    /// List all configured alloy IDs.
    pub fn list_alloys(&self) -> Vec<&AlloyProvider> {
        self.alloys.values().collect()
    }

    pub fn set_active_for_identity(&self, identity_id: &str, alloy_id: &str) -> Result<(), String> {
        if !self.alloys.contains_key(alloy_id) {
            return Err(format!("unknown alloy '{}'", alloy_id));
        }
        self.active_by_identity
            .lock()
            .expect("alloy manager mutex poisoned")
            .insert(identity_id.to_string(), alloy_id.to_string());
        Ok(())
    }

    pub fn clear_active_for_identity(&self, identity_id: &str) {
        self.active_by_identity
            .lock()
            .expect("alloy manager mutex poisoned")
            .remove(identity_id);
    }

    pub fn active_for_identity(&self, identity_id: &str) -> Option<String> {
        self.active_by_identity
            .lock()
            .expect("alloy manager mutex poisoned")
            .get(identity_id)
            .cloned()
    }

    pub fn select_plan_for_identity(&self, identity_id: &str) -> Option<AlloyPlan> {
        let alloy_id = self.active_for_identity(identity_id)?;
        let provider = self.alloys.get(&alloy_id)?;
        Some(provider.select_plan())
    }

    pub fn record_attempt(&self, alloy_id: &str, model: &str, success: bool) {
        if let Some(provider) = self.alloys.get(alloy_id) {
            provider.record_attempt(model, success);
        }
    }

    pub fn get(&self, alloy_id: &str) -> Option<&AlloyProvider> {
        self.alloys.get(alloy_id)
    }

    pub fn list(&self) -> Vec<&AlloyDefinition> {
        let mut v: Vec<&AlloyDefinition> = self.alloys.values().map(|p| p.definition()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_alloy(strategy: &str) -> AlloyConfig {
        AlloyConfig {
            id: "free-alloy-1".to_string(),
            name: "Free Alloy 1".to_string(),
            strategy: strategy.to_string(),
            constituents: vec![
                AlloyConstituentConfig {
                    model: "model-a".to_string(),
                    weight: 80,
                },
                AlloyConstituentConfig {
                    model: "model-b".to_string(),
                    weight: 20,
                },
            ],
        }
    }

    #[test]
    fn round_robin_rotates() {
        let p = AlloyProvider::from_config(&sample_alloy("round_robin")).unwrap();
        let first = p.select_plan().ordered_models;
        let second = p.select_plan().ordered_models;
        assert_eq!(first[0], "model-a");
        assert_eq!(second[0], "model-b");
    }

    #[test]
    fn weighted_returns_all_models() {
        let p = AlloyProvider::from_config(&sample_alloy("weighted")).unwrap();
        let plan = p.select_plan();
        assert_eq!(plan.ordered_models.len(), 2);
        assert!(plan.ordered_models.contains(&"model-a".to_string()));
        assert!(plan.ordered_models.contains(&"model-b".to_string()));
    }

    #[test]
    fn stats_are_recorded() {
        let p = AlloyProvider::from_config(&sample_alloy("round_robin")).unwrap();
        p.record_attempt("model-a", true);
        p.record_attempt("model-a", false);

        let stats = p.stats_snapshot();
        let a = stats.iter().find(|s| s.model == "model-a").unwrap();
        assert_eq!(a.requests, 2);
        assert_eq!(a.successes, 1);
        assert_eq!(a.failures, 1);
    }
}
