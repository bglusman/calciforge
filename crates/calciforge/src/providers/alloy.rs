//! Alloy provider runtime selection and analytics.
//!
//! This module implements weighted/round-robin constituent selection,
//! fallback ordering for graceful degradation, and per-constituent usage stats.

#![allow(dead_code)]

use crate::sync::Mutex;
use std::collections::HashMap;

use rand::distr::weighted::WeightedIndex;
use rand::distr::Distribution;
use rand::rng;
use serde::Serialize;

use crate::config::{
    AlloyConfig, AlloyConstituentConfig, CascadeConfig, DispatcherConfig, ExecModelConfig,
    SyntheticModelConfig,
};

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
    /// Declared context window (tokens) for this constituent.
    pub context_window: u32,
}

/// Immutable alloy definition.
#[derive(Debug, Clone)]
pub struct AlloyDefinition {
    pub id: String,
    pub name: String,
    pub strategy: AlloyStrategy,
    pub constituents: Vec<AlloyConstituent>,
    /// Effective minimum context window (tokens) the alloy will accept.
    ///
    /// Either the user's explicit `min_context_window` config or auto-computed
    /// as `min(constituent.context_window)` when unset. Always known since
    /// constituents' `context_window` is required at config load.
    pub min_context_window: u32,
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

/// One validated target inside a named cascade or dispatcher.
#[derive(Debug, Clone)]
pub struct SyntheticModelTarget {
    pub model: String,
    pub context_window: u32,
}

/// Runtime definition for an explicit ordered fallback chain.
#[derive(Debug, Clone)]
pub struct CascadeDefinition {
    pub id: String,
    pub name: String,
    pub models: Vec<SyntheticModelTarget>,
}

/// Runtime definition for a request-size-aware model selector.
#[derive(Debug, Clone)]
pub struct DispatcherDefinition {
    pub id: String,
    pub name: String,
    pub models: Vec<SyntheticModelTarget>,
}

/// Runtime definition for an executable-backed synthetic model.
#[derive(Debug, Clone)]
pub struct ExecModelDefinition {
    pub id: String,
    pub name: String,
    pub context_window: u32,
}

fn validate_synthetic_model(
    owner_kind: &str,
    owner_id: &str,
    c: &SyntheticModelConfig,
) -> Result<(), String> {
    if c.model.trim().is_empty() {
        return Err(format!(
            "{owner_kind} '{owner_id}': model must not be empty"
        ));
    }
    if c.context_window == 0 {
        return Err(format!(
            "{owner_kind} '{owner_id}': model '{}' context_window must be > 0",
            c.model
        ));
    }
    Ok(())
}

impl CascadeDefinition {
    pub fn from_config(cfg: &CascadeConfig) -> Result<Self, String> {
        if cfg.id.trim().is_empty() {
            return Err("cascade id must not be empty".to_string());
        }
        if cfg.models.is_empty() {
            return Err(format!(
                "cascade '{}': must define at least 1 model",
                cfg.id
            ));
        }
        let mut models = Vec::with_capacity(cfg.models.len());
        for c in &cfg.models {
            validate_synthetic_model("cascade", &cfg.id, c)?;
            models.push(SyntheticModelTarget {
                model: c.model.clone(),
                context_window: c.context_window,
            });
        }
        Ok(Self {
            id: cfg.id.clone(),
            name: cfg.name.clone().unwrap_or_else(|| cfg.id.clone()),
            models,
        })
    }

    /// Return configured targets in order, skipping models too small for the
    /// estimated request. That lets a cascade use local-first fallbacks without
    /// knowingly sending an oversized request to a small context window.
    pub fn select_plan(&self, estimated_tokens: u32) -> Result<AlloyPlan, String> {
        let ordered_models: Vec<String> = self
            .models
            .iter()
            .filter(|m| m.context_window >= estimated_tokens)
            .map(|m| m.model.clone())
            .collect();
        if ordered_models.is_empty() {
            let largest = self
                .models
                .iter()
                .map(|m| m.context_window)
                .max()
                .unwrap_or_default();
            return Err(format!(
                "cascade '{}': estimated request size {} tokens exceeds largest configured context window {}",
                self.id, estimated_tokens, largest
            ));
        }
        Ok(AlloyPlan {
            alloy_id: self.id.clone(),
            alloy_name: self.name.clone(),
            ordered_models,
        })
    }
}

impl DispatcherDefinition {
    pub fn from_config(cfg: &DispatcherConfig) -> Result<Self, String> {
        if cfg.id.trim().is_empty() {
            return Err("dispatcher id must not be empty".to_string());
        }
        if cfg.models.is_empty() {
            return Err(format!(
                "dispatcher '{}': must define at least 1 model",
                cfg.id
            ));
        }
        let mut models = Vec::with_capacity(cfg.models.len());
        for c in &cfg.models {
            validate_synthetic_model("dispatcher", &cfg.id, c)?;
            models.push(SyntheticModelTarget {
                model: c.model.clone(),
                context_window: c.context_window,
            });
        }
        Ok(Self {
            id: cfg.id.clone(),
            name: cfg.name.clone().unwrap_or_else(|| cfg.id.clone()),
            models,
        })
    }

    /// Choose the smallest eligible context window first, with larger models as
    /// fallbacks. This is intentionally different from alloys: dispatchers are
    /// for "use local while it fits, then move up" behavior.
    pub fn select_plan(&self, estimated_tokens: u32) -> Result<AlloyPlan, String> {
        let mut eligible: Vec<&SyntheticModelTarget> = self
            .models
            .iter()
            .filter(|m| m.context_window >= estimated_tokens)
            .collect();
        eligible.sort_by(|a, b| {
            a.context_window
                .cmp(&b.context_window)
                .then_with(|| a.model.cmp(&b.model))
        });

        if eligible.is_empty() {
            let largest = self
                .models
                .iter()
                .map(|m| m.context_window)
                .max()
                .unwrap_or_default();
            return Err(format!(
                "dispatcher '{}': estimated request size {} tokens exceeds largest configured context window {}",
                self.id, estimated_tokens, largest
            ));
        }

        Ok(AlloyPlan {
            alloy_id: self.id.clone(),
            alloy_name: self.name.clone(),
            ordered_models: eligible.into_iter().map(|m| m.model.clone()).collect(),
        })
    }
}

impl ExecModelDefinition {
    pub fn from_config(cfg: &ExecModelConfig) -> Result<Self, String> {
        if cfg.id.trim().is_empty() {
            return Err("exec model id must not be empty".to_string());
        }
        if cfg.context_window == 0 {
            return Err(format!(
                "exec model '{}': context_window must be > 0",
                cfg.id
            ));
        }
        if cfg.command.trim().is_empty() {
            return Err(format!(
                "exec model '{}': command must not be empty",
                cfg.id
            ));
        }
        Ok(Self {
            id: cfg.id.clone(),
            name: cfg.name.clone().unwrap_or_else(|| cfg.id.clone()),
            context_window: cfg.context_window,
        })
    }

    pub fn select_plan(&self, estimated_tokens: u32) -> Result<AlloyPlan, String> {
        if estimated_tokens > self.context_window {
            return Err(format!(
                "exec model '{}': estimated request size {} tokens exceeds context window {}",
                self.id, estimated_tokens, self.context_window
            ));
        }
        Ok(AlloyPlan {
            alloy_id: self.id.clone(),
            alloy_name: self.name.clone(),
            ordered_models: vec![self.id.clone()],
        })
    }
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

        if matches!(cfg.min_context_window, Some(0)) {
            return Err(format!(
                "alloy '{}': min_context_window must be > 0 when set (omit the field for auto-compute)",
                cfg.id
            ));
        }

        let mut constituents = Vec::with_capacity(cfg.constituents.len());
        for c in &cfg.constituents {
            validate_constituent(cfg, c)?;
            constituents.push(AlloyConstituent {
                model: c.model.clone(),
                weight: c.weight,
                context_window: c.context_window,
            });
        }

        // Context-window safety: if the user specified min_context_window, every
        // constituent must meet it. Otherwise auto-compute as min of declared sizes.
        // `context_window` is required on constituents (serde-enforced), so
        // declared_min always exists when constituents list is non-empty.
        let declared_min = constituents
            .iter()
            .map(|c| c.context_window)
            .min()
            .expect("validated above: constituents.len() >= 2");

        let effective_min = match cfg.min_context_window {
            Some(user_min) => {
                for c in &constituents {
                    if c.context_window < user_min {
                        return Err(format!(
                            "alloy '{}': constituent '{}' has context_window={} which is \
                             below the alloy's min_context_window={}. Either raise the \
                             constituent's declared size, lower the alloy's min, or remove \
                             the constituent.",
                            cfg.id, c.model, c.context_window, user_min
                        ));
                    }
                }
                user_min
            }
            None => declared_min,
        };

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
                min_context_window: effective_min,
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

    /// Effective minimum context window (tokens) this alloy can safely serve.
    ///
    /// Always known — constituents are required to declare `context_window`.
    pub fn min_context_window(&self) -> u32 {
        self.def.min_context_window
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
    if c.context_window == 0 {
        return Err(format!(
            "alloy '{}': constituent '{}' context_window must be > 0",
            alloy.id, c.model
        ));
    }
    Ok(())
}

fn validate_synthetic_dag(
    alloys: &HashMap<String, AlloyProvider>,
    cascades: &HashMap<String, CascadeDefinition>,
    dispatchers: &HashMap<String, DispatcherDefinition>,
    exec_models: &HashMap<String, ExecModelDefinition>,
) -> Result<(), String> {
    fn child_refs<'a>(
        id: &str,
        alloys: &'a HashMap<String, AlloyProvider>,
        cascades: &'a HashMap<String, CascadeDefinition>,
        dispatchers: &'a HashMap<String, DispatcherDefinition>,
        exec_models: &'a HashMap<String, ExecModelDefinition>,
    ) -> Vec<&'a str> {
        let is_synthetic = |model: &&String| {
            alloys.contains_key(*model)
                || cascades.contains_key(*model)
                || dispatchers.contains_key(*model)
                || exec_models.contains_key(*model)
        };

        if let Some(alloy) = alloys.get(id) {
            return alloy
                .definition()
                .constituents
                .iter()
                .map(|c| &c.model)
                .filter(is_synthetic)
                .map(String::as_str)
                .collect();
        }
        if let Some(cascade) = cascades.get(id) {
            return cascade
                .models
                .iter()
                .map(|m| &m.model)
                .filter(is_synthetic)
                .map(String::as_str)
                .collect();
        }
        if let Some(dispatcher) = dispatchers.get(id) {
            return dispatcher
                .models
                .iter()
                .map(|m| &m.model)
                .filter(is_synthetic)
                .map(String::as_str)
                .collect();
        }
        Vec::new()
    }

    fn visit(
        id: &str,
        alloys: &HashMap<String, AlloyProvider>,
        cascades: &HashMap<String, CascadeDefinition>,
        dispatchers: &HashMap<String, DispatcherDefinition>,
        exec_models: &HashMap<String, ExecModelDefinition>,
        visiting: &mut Vec<String>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }
        if let Some(pos) = visiting.iter().position(|current| current == id) {
            let mut cycle = visiting[pos..].to_vec();
            cycle.push(id.to_string());
            return Err(format!(
                "synthetic model graph contains a cycle: {}",
                cycle.join(" -> ")
            ));
        }

        visiting.push(id.to_string());
        for child in child_refs(id, alloys, cascades, dispatchers, exec_models) {
            visit(
                child,
                alloys,
                cascades,
                dispatchers,
                exec_models,
                visiting,
                visited,
            )?;
        }
        visiting.pop();
        visited.insert(id.to_string());
        Ok(())
    }

    let mut visited = std::collections::HashSet::new();
    let roots = alloys
        .keys()
        .chain(cascades.keys())
        .chain(dispatchers.keys())
        .chain(exec_models.keys());
    for id in roots {
        visit(
            id,
            alloys,
            cascades,
            dispatchers,
            exec_models,
            &mut Vec::new(),
            &mut visited,
        )?;
    }
    Ok(())
}

/// Manager for configured alloys and per-identity active alloy selection.
#[derive(Debug, Default)]
pub struct AlloyManager {
    alloys: HashMap<String, AlloyProvider>,
    cascades: HashMap<String, CascadeDefinition>,
    dispatchers: HashMap<String, DispatcherDefinition>,
    exec_models: HashMap<String, ExecModelDefinition>,
    active_by_identity: Mutex<HashMap<String, String>>,
}

impl Clone for AlloyManager {
    fn clone(&self) -> Self {
        Self {
            alloys: self.alloys.clone(),
            cascades: self.cascades.clone(),
            dispatchers: self.dispatchers.clone(),
            exec_models: self.exec_models.clone(),
            active_by_identity: Mutex::new(
                self.active_by_identity
                    .lock()
                    .expect("mutex poisoned")
                    .clone(),
            ),
        }
    }
}

impl AlloyManager {
    /// Create an empty alloy manager with no configured alloys.
    pub fn empty() -> Self {
        Self {
            alloys: HashMap::new(),
            cascades: HashMap::new(),
            dispatchers: HashMap::new(),
            exec_models: HashMap::new(),
            active_by_identity: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_configs(configs: &[AlloyConfig]) -> Result<Self, String> {
        Self::from_gateway_configs(configs, &[], &[], &[])
    }

    pub fn from_gateway_configs(
        configs: &[AlloyConfig],
        cascade_configs: &[CascadeConfig],
        dispatcher_configs: &[DispatcherConfig],
        exec_model_configs: &[ExecModelConfig],
    ) -> Result<Self, String> {
        let mut alloys = HashMap::new();
        for cfg in configs {
            let provider = AlloyProvider::from_config(cfg)?;
            if alloys.insert(cfg.id.clone(), provider).is_some() {
                return Err(format!("duplicate alloy id '{}'", cfg.id));
            }
        }

        let mut cascades = HashMap::new();
        for cfg in cascade_configs {
            let cascade = CascadeDefinition::from_config(cfg)?;
            if alloys.contains_key(&cfg.id) || cascades.insert(cfg.id.clone(), cascade).is_some() {
                return Err(format!("duplicate synthetic model id '{}'", cfg.id));
            }
        }

        let mut dispatchers = HashMap::new();
        for cfg in dispatcher_configs {
            let dispatcher = DispatcherDefinition::from_config(cfg)?;
            if alloys.contains_key(&cfg.id)
                || cascades.contains_key(&cfg.id)
                || dispatchers.insert(cfg.id.clone(), dispatcher).is_some()
            {
                return Err(format!("duplicate synthetic model id '{}'", cfg.id));
            }
        }

        let mut exec_models = HashMap::new();
        for cfg in exec_model_configs {
            let exec_model = ExecModelDefinition::from_config(cfg)?;
            if alloys.contains_key(&cfg.id)
                || cascades.contains_key(&cfg.id)
                || dispatchers.contains_key(&cfg.id)
                || exec_models.insert(cfg.id.clone(), exec_model).is_some()
            {
                return Err(format!("duplicate synthetic model id '{}'", cfg.id));
            }
        }

        validate_synthetic_dag(&alloys, &cascades, &dispatchers, &exec_models)?;

        Ok(Self {
            alloys,
            cascades,
            dispatchers,
            exec_models,
            active_by_identity: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.alloys.is_empty()
            && self.cascades.is_empty()
            && self.dispatchers.is_empty()
            && self.exec_models.is_empty()
    }

    /// Get an alloy by ID.
    pub fn get_alloy(&self, alloy_id: &str) -> Option<&AlloyProvider> {
        self.alloys.get(alloy_id)
    }

    pub fn is_synthetic_model(&self, model_id: &str) -> bool {
        self.alloys.contains_key(model_id)
            || self.cascades.contains_key(model_id)
            || self.dispatchers.contains_key(model_id)
            || self.exec_models.contains_key(model_id)
    }

    pub fn select_plan_for_model(
        &self,
        model_id: &str,
        estimated_tokens: u32,
    ) -> Result<Option<AlloyPlan>, String> {
        self.select_plan_recursive(model_id, estimated_tokens, &mut Vec::new())
    }

    fn select_plan_recursive(
        &self,
        model_id: &str,
        estimated_tokens: u32,
        stack: &mut Vec<String>,
    ) -> Result<Option<AlloyPlan>, String> {
        if stack.iter().any(|id| id == model_id) {
            stack.push(model_id.to_string());
            return Err(format!(
                "synthetic model cycle detected: {}",
                stack.join(" -> ")
            ));
        }

        if let Some(alloy) = self.alloys.get(model_id) {
            let min_context_window = alloy.min_context_window();
            if estimated_tokens > min_context_window {
                return Err(format!(
                    "alloy '{}': estimated request size {} tokens exceeds effective context window {}",
                    model_id, estimated_tokens, min_context_window
                ));
            }
            stack.push(model_id.to_string());
            let plan = self.expand_plan(alloy.select_plan(), estimated_tokens, stack)?;
            stack.pop();
            return Ok(Some(plan));
        }
        if let Some(cascade) = self.cascades.get(model_id) {
            stack.push(model_id.to_string());
            let plan = self.expand_plan(
                cascade.select_plan(estimated_tokens)?,
                estimated_tokens,
                stack,
            )?;
            stack.pop();
            return Ok(Some(plan));
        }
        if let Some(dispatcher) = self.dispatchers.get(model_id) {
            stack.push(model_id.to_string());
            let plan = self.expand_plan(
                dispatcher.select_plan(estimated_tokens)?,
                estimated_tokens,
                stack,
            )?;
            stack.pop();
            return Ok(Some(plan));
        }
        if let Some(exec_model) = self.exec_models.get(model_id) {
            return exec_model.select_plan(estimated_tokens).map(Some);
        }
        Ok(None)
    }

    fn expand_plan(
        &self,
        mut plan: AlloyPlan,
        estimated_tokens: u32,
        stack: &mut Vec<String>,
    ) -> Result<AlloyPlan, String> {
        let mut expanded = Vec::new();
        for target in &plan.ordered_models {
            match self.select_plan_recursive(target, estimated_tokens, stack)? {
                Some(child) => expanded.extend(child.ordered_models),
                None => expanded.push(target.clone()),
            }
        }
        plan.ordered_models = expanded;
        Ok(plan)
    }

    /// List all configured alloy IDs.
    pub fn list_alloys(&self) -> Vec<&AlloyProvider> {
        self.alloys.values().collect()
    }

    pub fn list_cascades(&self) -> Vec<&CascadeDefinition> {
        let mut v: Vec<&CascadeDefinition> = self.cascades.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn list_dispatchers(&self) -> Vec<&DispatcherDefinition> {
        let mut v: Vec<&DispatcherDefinition> = self.dispatchers.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn list_exec_models(&self) -> Vec<&ExecModelDefinition> {
        let mut v: Vec<&ExecModelDefinition> = self.exec_models.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn set_active_for_identity(&self, identity_id: &str, model_id: &str) -> Result<(), String> {
        if !self.is_synthetic_model(model_id) {
            return Err(format!("unknown synthetic model '{}'", model_id));
        }
        self.active_by_identity
            .lock()
            .expect("alloy manager mutex poisoned")
            .insert(identity_id.to_string(), model_id.to_string());
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
    use crate::config::{CascadeConfig, DispatcherConfig, ExecModelConfig, SyntheticModelConfig};

    fn sample_alloy(strategy: &str) -> AlloyConfig {
        AlloyConfig {
            id: "free-alloy-1".to_string(),
            name: "Free Alloy 1".to_string(),
            strategy: strategy.to_string(),
            constituents: vec![
                AlloyConstituentConfig {
                    model: "model-a".to_string(),
                    weight: 80,
                    context_window: 128_000,
                },
                AlloyConstituentConfig {
                    model: "model-b".to_string(),
                    weight: 20,
                    context_window: 128_000,
                },
            ],
            min_context_window: None,
        }
    }

    fn alloy_with_sizes(sizes: &[(&str, u32, u32)], min_cw: Option<u32>) -> AlloyConfig {
        AlloyConfig {
            id: "sized".to_string(),
            name: "Sized Alloy".to_string(),
            strategy: "weighted".to_string(),
            constituents: sizes
                .iter()
                .map(|(model, w, cw)| AlloyConstituentConfig {
                    model: (*model).to_string(),
                    weight: *w,
                    context_window: *cw,
                })
                .collect(),
            min_context_window: min_cw,
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

    #[test]
    fn auto_computes_min_context_window_from_constituents() {
        let cfg = alloy_with_sizes(
            &[("big-model", 50, 262_144), ("small-model", 50, 32_768)],
            None,
        );
        let p = AlloyProvider::from_config(&cfg).unwrap();
        assert_eq!(p.min_context_window(), 32_768);
    }

    #[test]
    fn equal_sized_constituents_use_shared_context_window() {
        let p = AlloyProvider::from_config(&sample_alloy("weighted")).unwrap();
        // sample_alloy constituents both declare 128K
        assert_eq!(p.min_context_window(), 128_000);
    }

    #[test]
    fn explicit_min_takes_priority_over_auto() {
        let cfg = alloy_with_sizes(
            &[("big", 50, 262_144), ("bigger", 50, 1_000_000)],
            Some(100_000),
        );
        let p = AlloyProvider::from_config(&cfg).unwrap();
        assert_eq!(p.min_context_window(), 100_000);
    }

    #[test]
    fn rejects_constituent_below_explicit_min() {
        let cfg = alloy_with_sizes(
            &[("kimi-k2", 50, 262_144), ("local-qwen", 50, 32_768)],
            Some(200_000),
        );
        let err = AlloyProvider::from_config(&cfg).unwrap_err();
        assert!(
            err.contains("local-qwen") && err.contains("32768") && err.contains("200000"),
            "expected explanatory error naming the offending constituent, got: {err}"
        );
    }

    #[test]
    fn rejects_zero_context_window_on_constituent() {
        let cfg = alloy_with_sizes(&[("a", 50, 0), ("b", 50, 128_000)], None);
        let err = AlloyProvider::from_config(&cfg).unwrap_err();
        assert!(
            err.contains("context_window must be > 0"),
            "expected explanatory error, got: {err}"
        );
    }

    #[test]
    fn rejects_zero_min_context_window() {
        let cfg = alloy_with_sizes(&[("a", 50, 128_000), ("b", 50, 128_000)], Some(0));
        let err = AlloyProvider::from_config(&cfg).unwrap_err();
        assert!(
            err.contains("min_context_window must be > 0"),
            "expected explanatory error, got: {err}"
        );
    }

    #[test]
    fn cascade_skips_models_too_small_for_request() {
        let cascade = CascadeDefinition::from_config(&CascadeConfig {
            id: "local-then-remote".to_string(),
            name: None,
            models: vec![
                SyntheticModelConfig {
                    model: "local/small".to_string(),
                    context_window: 32_768,
                },
                SyntheticModelConfig {
                    model: "remote/large".to_string(),
                    context_window: 262_144,
                },
            ],
        })
        .unwrap();

        let plan = cascade.select_plan(100_000).unwrap();
        assert_eq!(plan.ordered_models, vec!["remote/large"]);
    }

    #[test]
    fn dispatcher_chooses_smallest_model_that_fits_first() {
        let dispatcher = DispatcherDefinition::from_config(&DispatcherConfig {
            id: "smart-local".to_string(),
            name: None,
            models: vec![
                SyntheticModelConfig {
                    model: "remote/large".to_string(),
                    context_window: 262_144,
                },
                SyntheticModelConfig {
                    model: "local/small".to_string(),
                    context_window: 32_768,
                },
                SyntheticModelConfig {
                    model: "local/medium".to_string(),
                    context_window: 65_536,
                },
            ],
        })
        .unwrap();

        let plan = dispatcher.select_plan(40_000).unwrap();
        assert_eq!(plan.ordered_models, vec!["local/medium", "remote/large"]);
    }

    #[test]
    fn manager_rejects_duplicate_synthetic_ids() {
        let alloy = sample_alloy("weighted");
        let dispatcher = DispatcherConfig {
            id: alloy.id.clone(),
            name: None,
            models: vec![SyntheticModelConfig {
                model: "local/small".to_string(),
                context_window: 32_768,
            }],
        };
        let err =
            AlloyManager::from_gateway_configs(&[alloy], &[], &[dispatcher], &[]).unwrap_err();
        assert!(
            err.contains("duplicate synthetic model id"),
            "expected duplicate id error, got: {err}"
        );
    }

    #[test]
    fn manager_rejects_alloy_plan_that_exceeds_effective_context_window() {
        let alloy = alloy_with_sizes(&[("small", 50, 32_768), ("large", 50, 262_144)], None);
        let manager = AlloyManager::from_gateway_configs(&[alloy], &[], &[], &[]).unwrap();

        let err = manager.select_plan_for_model("sized", 40_000).unwrap_err();
        assert!(
            err.contains("sized") && err.contains("40000") && err.contains("32768"),
            "expected context-window error naming the alloy and limits, got: {err}"
        );
    }

    #[test]
    fn exec_model_is_a_synthetic_leaf_with_context_limit() {
        let exec = ExecModelConfig {
            id: "codex/gpt-5.5".to_string(),
            name: Some("Codex GPT-5.5".to_string()),
            context_window: 262_144,
            command: "codex".to_string(),
            args: vec![
                "exec".to_string(),
                "-m".to_string(),
                "gpt-5.5".to_string(),
                "-".to_string(),
            ],
            env: HashMap::new(),
            timeout_seconds: Some(900),
        };
        let manager = AlloyManager::from_gateway_configs(&[], &[], &[], &[exec]).unwrap();

        let plan = manager
            .select_plan_for_model("codex/gpt-5.5", 100_000)
            .unwrap()
            .unwrap();
        assert_eq!(plan.ordered_models, vec!["codex/gpt-5.5"]);

        let err = manager
            .select_plan_for_model("codex/gpt-5.5", 300_000)
            .unwrap_err();
        assert!(err.contains("context window"), "{err}");
    }

    #[test]
    fn synthetic_models_can_compose_as_a_dag() {
        let exec = ExecModelConfig {
            id: "codex/gpt-5.5".to_string(),
            name: None,
            context_window: 262_144,
            command: "codex".to_string(),
            args: vec![],
            env: HashMap::new(),
            timeout_seconds: None,
        };
        let cascade = CascadeConfig {
            id: "local-then-codex".to_string(),
            name: None,
            models: vec![
                SyntheticModelConfig {
                    model: "local/small".to_string(),
                    context_window: 32_768,
                },
                SyntheticModelConfig {
                    model: "codex/gpt-5.5".to_string(),
                    context_window: 262_144,
                },
            ],
        };
        let dispatcher = DispatcherConfig {
            id: "smart".to_string(),
            name: None,
            models: vec![SyntheticModelConfig {
                model: "local-then-codex".to_string(),
                context_window: 262_144,
            }],
        };
        let manager =
            AlloyManager::from_gateway_configs(&[], &[cascade], &[dispatcher], &[exec]).unwrap();

        let plan = manager
            .select_plan_for_model("smart", 40_000)
            .unwrap()
            .unwrap();
        assert_eq!(plan.ordered_models, vec!["codex/gpt-5.5"]);
    }

    #[test]
    fn manager_rejects_synthetic_cycles() {
        let cascade = CascadeConfig {
            id: "a".to_string(),
            name: None,
            models: vec![SyntheticModelConfig {
                model: "b".to_string(),
                context_window: 128_000,
            }],
        };
        let dispatcher = DispatcherConfig {
            id: "b".to_string(),
            name: None,
            models: vec![SyntheticModelConfig {
                model: "a".to_string(),
                context_window: 128_000,
            }],
        };

        let err =
            AlloyManager::from_gateway_configs(&[], &[cascade], &[dispatcher], &[]).unwrap_err();
        assert!(err.contains("cycle"), "{err}");
    }
}
