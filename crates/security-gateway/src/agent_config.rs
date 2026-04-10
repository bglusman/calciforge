use serde::{Deserialize, Serialize};

/// Provider mapping for credential injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub env_key: String,
}

/// Proxy enforcement policy for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPolicy {
    pub enforcement: EnforcementMode,
    pub scan_outbound: bool,
    pub scan_inbound: bool,
    pub inject_credentials: bool,
}

impl Default for ProxyPolicy {
    fn default() -> Self {
        Self {
            enforcement: EnforcementMode::EnvVar,
            scan_outbound: true,
            scan_inbound: true,
            inject_credentials: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementMode {
    /// Set HTTP_PROXY/HTTPS_PROXY env vars (Tier 1)
    EnvVar,
    /// iptables redirect (Tier 2, not yet implemented)
    Firewall,
    /// Network namespace isolation (Tier 3, not yet implemented)
    Namespace,
}

/// Domain list source for threat feeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainListSource {
    pub name: String,
    pub url: String,
    pub refresh_secs: u64,
}

/// Agent configuration (mirrors clashd's agents.json format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub agent_id: String,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub denied_domains: Vec<String>,
    #[serde(default)]
    pub domain_list_sources: Vec<DomainListSource>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub proxy: ProxyPolicy,
}

/// Top-level agents config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    pub agents: Vec<AgentConfig>,
}

impl AgentsConfig {
    /// Load from a JSON file path.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: AgentsConfig = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Get a specific agent by ID.
    pub fn agent(&self, id: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.agent_id == id)
    }

    /// Get all providers across all agents.
    pub fn all_providers(&self) -> Vec<&ProviderConfig> {
        self.agents
            .iter()
            .flat_map(|a| a.providers.iter())
            .collect()
    }
}
