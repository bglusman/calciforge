//! Provider-layer modules used by Calciforge command/runtime orchestration.

pub mod alloy;

use crate::sync::RwLock;
use std::collections::HashMap;

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

#[cfg(all(test, loom))]
mod loom_tests {
    use super::*;
    use loom::thread;

    #[test]
    fn test_concurrent_provider_registry_access() {
        loom::model(|| {
            let registry = ProviderRegistry::new();

            // Create provider info for testing
            let provider1 = ProviderInfo {
                id: "provider1".to_string(),
                name: "Test Provider 1".to_string(),
                base_url: "http://example.com".to_string(),
                api_key_env: None,
                supports_streaming: true,
                supports_tools: true,
            };

            let provider2 = ProviderInfo {
                id: "provider2".to_string(),
                name: "Test Provider 2".to_string(),
                base_url: "http://example2.com".to_string(),
                api_key_env: None,
                supports_streaming: false,
                supports_tools: true,
            };

            // Thread 1: Register providers
            let registry1 = registry.clone();
            let p1 = provider1.clone();
            let p2 = provider2.clone();
            let t1 = thread::spawn(move || {
                registry1.register(p1);
                registry1.register(p2);
            });

            // Thread 2: Read while writing
            let registry2 = registry.clone();
            let t2 = thread::spawn(move || {
                // This might read empty or partial state
                let _ids = registry2.list_ids();
                let _provider = registry2.get("provider1");
            });

            // Thread 3: More reads
            let registry3 = registry.clone();
            let t3 = thread::spawn(move || {
                let _ids = registry3.list_ids();
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();

            // After all threads join, verify final state
            assert!(registry.get("provider1").is_some());
            assert!(registry.get("provider2").is_some());
            let ids = registry.list_ids();
            assert!(ids.contains(&"provider1".to_string()));
            assert!(ids.contains(&"provider2".to_string()));
        });
    }

    #[test]
    fn test_provider_registry_read_only() {
        loom::model(|| {
            let registry = ProviderRegistry::new();

            // Pre-populate with some providers
            registry.register(ProviderInfo {
                id: "openai".to_string(),
                name: "OpenAI".to_string(),
                base_url: "https://api.openai.com".to_string(),
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                supports_streaming: true,
                supports_tools: true,
            });

            registry.register(ProviderInfo {
                id: "anthropic".to_string(),
                name: "Anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                supports_streaming: true,
                supports_tools: true,
            });

            // Multiple concurrent readers
            let registry1 = registry.clone();
            let registry2 = registry.clone();
            let registry3 = registry.clone();

            let t1 = thread::spawn(move || {
                let provider = registry1.get("openai");
                assert!(provider.is_some());
                if let Some(p) = provider {
                    assert_eq!(p.id, "openai");
                }
            });

            let t2 = thread::spawn(move || {
                let ids = registry2.list_ids();
                assert!(ids.contains(&"openai".to_string()));
                assert!(ids.contains(&"anthropic".to_string()));
            });

            let t3 = thread::spawn(move || {
                let provider = registry3.get("anthropic");
                assert!(provider.is_some());
                if let Some(p) = provider {
                    assert_eq!(p.id, "anthropic");
                }
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    }
}
