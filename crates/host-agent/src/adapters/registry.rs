//! AdapterRegistry — maps operation kind strings to boxed Adapter implementations.

use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

use super::Adapter;

/// Central registry of adapters.  Thread-safe and cheaply cloneable.
#[derive(Clone)]
pub struct AdapterRegistry {
    adapters: Arc<HashMap<String, Arc<dyn Adapter>>>,
}

impl AdapterRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self {
            adapters: Arc::new(HashMap::new()),
        }
    }

    /// Builder: register an adapter.  Panics if the kind is already registered.
    pub fn with(mut self, adapter: impl Adapter + 'static) -> Self {
        let kind = adapter.kind().to_string();
        let map = Arc::make_mut(&mut self.adapters);
        if map.contains_key(&kind) {
            panic!("Duplicate adapter kind: {kind}");
        }
        map.insert(kind, Arc::new(adapter));
        self
    }

    /// Look up an adapter by kind.  Returns `None` for unknown kinds.
    pub fn get(&self, kind: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters.get(kind).cloned()
    }

    /// List registered kinds.
    pub fn kinds(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }

    /// Warn-and-return-None if a kind is requested that was never registered.
    pub fn dispatch(&self, kind: &str) -> Option<Arc<dyn Adapter>> {
        let adapter = self.adapters.get(kind).cloned();
        if adapter.is_none() {
            warn!(kind = %kind, "No adapter registered for kind");
        }
        adapter
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{Adapter, AppError, ExecutionResult, HostOp, PolicyDecision};
    use crate::auth::ClientIdentity;
    use crate::AppState;
    use async_trait::async_trait;

    struct FooAdapter;

    #[async_trait]
    impl Adapter for FooAdapter {
        fn kind(&self) -> &'static str {
            "foo"
        }

        async fn validate(&self, _: &AppState, _: &HostOp) -> Result<PolicyDecision, AppError> {
            Ok(PolicyDecision::Allow)
        }

        async fn execute(
            &self,
            _: &AppState,
            _: &ClientIdentity,
            _: &HostOp,
        ) -> Result<ExecutionResult, AppError> {
            Ok(ExecutionResult::ok("foo"))
        }
    }

    struct BarAdapter;

    #[async_trait]
    impl Adapter for BarAdapter {
        fn kind(&self) -> &'static str {
            "bar"
        }

        async fn validate(&self, _: &AppState, _: &HostOp) -> Result<PolicyDecision, AppError> {
            Ok(PolicyDecision::Allow)
        }

        async fn execute(
            &self,
            _: &AppState,
            _: &ClientIdentity,
            _: &HostOp,
        ) -> Result<ExecutionResult, AppError> {
            Ok(ExecutionResult::ok("bar"))
        }
    }

    #[test]
    fn test_registry_dispatch_known() {
        let reg = AdapterRegistry::new().with(FooAdapter).with(BarAdapter);
        assert!(reg.dispatch("foo").is_some());
        assert!(reg.dispatch("bar").is_some());
    }

    #[test]
    fn test_registry_dispatch_unknown() {
        let reg = AdapterRegistry::new().with(FooAdapter);
        assert!(reg.dispatch("baz").is_none());
    }

    #[test]
    fn test_registry_kinds() {
        let reg = AdapterRegistry::new().with(FooAdapter).with(BarAdapter);
        let mut kinds = reg.kinds();
        kinds.sort();
        assert_eq!(kinds, vec!["bar", "foo"]);
    }

    #[test]
    #[should_panic(expected = "Duplicate adapter kind: foo")]
    fn test_registry_duplicate_panics() {
        AdapterRegistry::new().with(FooAdapter).with(FooAdapter);
    }
}
