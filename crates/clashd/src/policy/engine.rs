//! Policy execution engine
//!
//! Combines the Starlark evaluator with domain lists and per-agent config
//! to provide centralized policy enforcement for OpenClaw tool calls.

use std::collections::HashMap;
use std::path::Path;
use tokio::sync::RwLock;
use tracing::{info, warn};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain_lists::DomainListManager;
use crate::policy::eval::PolicyEvaluator;
use crate::policy::PolicyResult;

#[cfg(test)]
mod tests;

/// Per-agent policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPolicyConfig {
    /// Agent identifier (e.g., "librarian", "custodian")
    pub agent_id: String,
    /// Additional allowed domains for this agent
    pub allowed_domains: Vec<String>,
    /// Additional denied domains for this agent
    pub denied_domains: Vec<String>,
    /// Additional domain list sources (URLs of threat feeds)
    pub domain_list_sources: Vec<DomainListSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainListSource {
    pub name: String,
    pub url: String,
    /// Refresh interval in seconds
    pub refresh_secs: u64,
}

/// Central policy engine that integrates Starlark evaluation with
/// domain checking and per-agent scoping
pub struct PolicyEngine {
    evaluator: PolicyEvaluator,
    domain_manager: DomainListManager,
    agent_configs: RwLock<HashMap<String, AgentPolicyConfig>>,
}

impl PolicyEngine {
    /// Create a new policy engine
    pub async fn new(policy_path: &Path) -> anyhow::Result<Self> {
        let evaluator = PolicyEvaluator::new(policy_path).await?;
        let domain_manager = DomainListManager::new();

        Ok(Self {
            evaluator,
            domain_manager,
            agent_configs: RwLock::new(HashMap::new()),
        })
    }

    /// Set per-agent configurations
    pub async fn set_agent_configs(&self, configs: Vec<AgentPolicyConfig>) {
        let mut map = self.agent_configs.write().await;
        for config in configs {
            map.insert(config.agent_id.clone(), config);
        }
    }

    /// Evaluate a tool call with full policy context
    pub async fn evaluate(&self, tool: &str, args: &Value, agent_id: Option<&str>) -> PolicyResult {
        // Build the context object for Starlark
        let mut context = serde_json::json!({
            "tool": tool,
            "args": args,
        });

        // Add agent-specific context
        if let Some(agent_id) = agent_id {
            context["agent_id"] = Value::String(agent_id.to_string());

            // Check if this tool call involves a domain
            let domain = Self::extract_domain(args);
            if let Some(ref domain_str) = domain {
                // Check against dynamic domain lists
                let matched = self.domain_manager.matches(domain_str).await;
                context["domain_lists"] =
                    Value::Array(matched.iter().map(|s| Value::String(s.clone())).collect());

                // Check per-agent allow/deny lists
                if let Some(config) = self.agent_configs.read().await.get(agent_id) {
                    context["agent_allowed_domains"] = Value::Array(
                        config
                            .allowed_domains
                            .iter()
                            .map(|s| Value::String(s.clone()))
                            .collect(),
                    );
                    context["agent_denied_domains"] = Value::Array(
                        config
                            .denied_domains
                            .iter()
                            .map(|s| Value::String(s.clone()))
                            .collect(),
                    );
                }
            }
            context["domain"] = domain.map(Value::String).unwrap_or(Value::Null);
        }

        // Evaluate through Starlark
        match self.evaluator.evaluate(tool, args, Some(&context)).await {
            Ok(result) => {
                info!(agent = ?agent_id, tool, verdict = %result.verdict, reason = ?result.reason, "Policy decision");
                result
            }
            Err(e) => {
                warn!(agent = ?agent_id, tool, error = %e, "Policy evaluation failed, defaulting to deny");
                // On error, default to deny for safety
                PolicyResult::deny(format!("Policy evaluation error: {}", e))
            }
        }
    }

    /// Refresh all domain lists
    pub async fn refresh_domain_lists(&self, client: &reqwest::Client) -> anyhow::Result<()> {
        self.domain_manager.refresh_all(client).await
    }

    /// Get summary of loaded domain lists
    pub async fn domain_list_summary(&self) -> Vec<(String, usize)> {
        self.domain_manager.summary().await
    }

    /// Extract a domain from common argument patterns
    ///
    /// Looks for domain in args.url, args.domain, args.target, etc.
    #[cfg(test)]
    pub fn extract_domain(args: &Value) -> Option<String> {
        Self::_extract_domain(args)
    }

    #[cfg(not(test))]
    fn extract_domain(args: &Value) -> Option<String> {
        Self::_extract_domain(args)
    }

    fn _extract_domain(args: &Value) -> Option<String> {
        if let Some(obj) = args.as_object() {
            // Try common field names — destructure to get &str from &&str
            for &field in &["url", "domain", "target", "host", "site"] {
                if let Some(val) = obj.get(field).and_then(|v| v.as_str()) {
                    if let Some(domain) = Self::parse_domain(val) {
                        return Some(domain);
                    }
                }
            }
        }
        None
    }

    /// Parse a domain from a URL or plain string
    fn parse_domain(s: &str) -> Option<String> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Try to parse as URL
        if let Ok(url) = url::Url::parse(s) {
            return url.host_str().map(|h| h.to_string());
        }

        // Plain domain string — if it looks like a domain
        if s.contains('.') && !s.contains(' ') {
            // Strip any port
            let domain = s.split(':').next().unwrap_or(s);
            // Strip protocol prefix if present
            let domain = domain
                .trim_start_matches("http://")
                .trim_start_matches("https://");
            return Some(domain.to_lowercase());
        }

        None
    }
}
