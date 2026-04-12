//! Provider-layer modules used by ZeroClawed command/runtime orchestration.

pub mod alloy;

use std::collections::HashMap;
use std::sync::RwLock;

/// Registry of available model providers.
/// Maps model IDs to their provider endpoints and capabilities.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, ProviderInfo>>,
}

/// Information about a single provider.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub supports_streaming: bool,
    pub supports_tools: bool,
}

#[allow(dead_code)]
impl ProviderRegistry {
    /// Create a new empty provider registry.
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a provider.
    pub fn register(&self, info: ProviderInfo) {
        let mut providers = self.providers.write().expect("registry lock poisoned");
        providers.insert(info.id.clone(), info);
    }

    /// Get a provider by ID.
    pub fn get(&self, id: &str) -> Option<ProviderInfo> {
        let providers = self.providers.read().expect("registry lock poisoned");
        providers.get(id).cloned()
    }

    /// List all registered provider IDs.
    pub fn list_ids(&self) -> Vec<String> {
        let providers = self.providers.read().expect("registry lock poisoned");
        providers.keys().cloned().collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
