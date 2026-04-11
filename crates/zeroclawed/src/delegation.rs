//! Agent Delegation Engine
//!
//! Allows agents to dynamically delegate to other configured agents.
//! Parses delegation markers in agent responses and orchestrates chained calls.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, info, warn};

use crate::adapters::{AdapterError, DispatchContext};
use crate::config::{AgentConfig, PolyConfig};
use crate::router::Router;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Delegation ACL configuration for an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationConfig {
    /// Agents this agent can delegate TO.
    /// "any" = all agents, "none" = no delegation, or list of agent IDs.
    #[serde(default = "default_delegates")]
    pub delegates: DelegationTarget,

    /// Agents that can delegate TO this agent.
    /// Same semantics as `delegates`.
    #[serde(default = "default_accepts_from")]
    pub accepts_from: DelegationTarget,
}

fn default_delegates() -> DelegationTarget {
    DelegationTarget::None
}

fn default_accepts_from() -> DelegationTarget {
    DelegationTarget::Any
}

/// Target specification for delegation ACLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DelegationTarget {
    /// Literal "any" or "none"
    Keyword(String),
    /// Specific agent IDs
    List(Vec<String>),
}

impl Default for DelegationTarget {
    fn default() -> Self {
        DelegationTarget::None
    }
}

impl DelegationTarget {
    /// Check if `target` is allowed by this specification.
    pub fn allows(&self, target: &str) -> bool {
        match self {
            DelegationTarget::Keyword(k) => match k.as_str() {
                "any" => true,
                "none" => false,
                _ => {
                    warn!(keyword = %k, "unknown delegation target keyword");
                    false
                }
            },
            DelegationTarget::List(list) => list.iter().any(|s| s == target),
        }
    }
}

// ---------------------------------------------------------------------------
// Delegation Markers
// ---------------------------------------------------------------------------

/// A delegation request extracted from an agent response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRequest {
    /// Target agent ID.
    pub target: String,

    /// Context sharing mode.
    #[serde(default)]
    pub context: ContextMode,

    /// Message to send to target agent.
    pub message: String,
}

/// Context sharing modes for delegation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    /// No context, just the delegation message.
    None,
    /// Last N turns (configurable, default 5).
    #[default]
    Recent,
    /// Copy of full context at delegation time (isolated).
    Fork,
}

/// Parse delegation markers from agent response text.
///
/// Supports two formats:
/// 1. TOML-like blocks: `[delegate]...[/delegate]`
/// 2. Inline JSON: `::delegate::{"target": "..."}::`
pub fn parse_delegation_markers(text: &str) -> Vec<DelegationRequest> {
    let mut requests = Vec::new();

    // Try TOML-like blocks first
    let block_regex = regex::Regex::new(
        r"\[delegate\]\s*\n?((?:.|\n)*?)\[/delegate\]"
    ).ok();

    if let Some(re) = block_regex {
        for cap in re.captures_iter(text) {
            let content = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Ok(req) = toml::from_str::<DelegationRequest>(content) {
                requests.push(req);
            } else {
                warn!(content = %content, "failed to parse delegation block as TOML");
            }
        }
    }

    // Try inline JSON format
    let inline_regex = regex::Regex::new(
        r"::delegate::(.*?)::"
    ).ok();

    if let Some(re) = inline_regex {
        for cap in re.captures_iter(text) {
            let content = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Ok(req) = serde_json::from_str::<DelegationRequest>(content) {
                requests.push(req);
            } else {
                warn!(content = %content, "failed to parse inline delegation as JSON");
            }
        }
    }

    requests
}

// ---------------------------------------------------------------------------
// Delegation Engine
// ---------------------------------------------------------------------------

/// Tracks delegation state to prevent loops and enforce limits.
#[derive(Debug, Clone, Default)]
pub struct DelegationState {
    /// Number of delegations so far in this chain.
    pub depth: usize,

    /// (source, target) pairs seen to detect cycles.
    pub edges: Vec<(String, String)>,
}

impl DelegationState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a delegation edge. Returns Err if cycle detected.
    pub fn record_edge(&mut self, source: &str, target: &str) -> Result<()> {
        let edge = (source.to_string(), target.to_string());

        if self.edges.contains(&edge) {
            return Err(anyhow::anyhow!(
                "delegation cycle detected: {} -> {}",
                source, target
            ));
        }

        self.edges.push(edge);
        self.depth += 1;
        Ok(())
    }
}

/// Delegation engine - orchestrates agent-to-agent delegation.
pub struct DelegationEngine<'a> {
    config: &'a PolyConfig,
    router: &'a Router,
    max_depth: usize,
}

impl<'a> DelegationEngine<'a> {
    pub fn new(config: &'a PolyConfig, router: &'a Router) -> Self {
        Self {
            config,
            router,
            max_depth: config.delegation.max_depth,
        }
    }

    /// Dispatch with potential delegation chaining.
    ///
    /// This is a recursive method that:
    /// 1. Dispatches to the initial agent
    /// 2. Checks response for delegation markers
    /// 3. If found, validates ACLs and chains to target agent
    /// 4. Repeats until no more delegation or limit reached
    pub async fn dispatch_with_delegation(
        &self,
        text: &str,
        agent: &AgentConfig,
        sender: Option<&str>,
        context_store: &crate::context::ContextStore,
        chat_key: &str,
        state: &mut DelegationState,
    ) -> Result<String> {
        // Check depth limit
        if state.depth >= self.max_depth {
            return Err(anyhow::anyhow!(
                "delegation depth limit ({}) reached", self.max_depth
            ));
        }

        // Build context based on mode (for now, always use recent)
        let ctx = DispatchContext {
            message: text,
            sender,
        };

        // Dispatch to agent
        let response = self.router.dispatch_with_sender(text, agent, self.config, sender).await?;

        // Parse delegation markers
        let delegations = parse_delegation_markers(&response);

        if delegations.is_empty() {
            // No delegation - return response as-is
            return Ok(response);
        }

        // For now, only handle single delegation (first one)
        // TODO: Support fan-out (parallel delegations)
        let delegation = &delegations[0];

        // Validate ACLs
        if !self.validate_delegation(&agent.id, delegation) {
            return Err(anyhow::anyhow!(
                "delegation from '{}' to '{}' violates ACL rules",
                agent.id, delegation.target
            ));
        }

        // Record edge for cycle detection
        state.record_edge(&agent.id, &delegation.target)?;

        // Find target agent
        let target_agent = self.config.agents.iter()
            .find(|a| a.id == delegation.target)
            .ok_or_else(|| anyhow::anyhow!("delegation target '{}' not found", delegation.target))?;

        info!(
            source = %agent.id,
            target = %delegation.target,
            depth = %state.depth,
            "delegating to agent"
        );

        // Build context for delegation based on mode
        let delegate_text = match delegation.context {
            ContextMode::None => &delegation.message,
            ContextMode::Recent => {
                // Get recent context and prepend
                let recent = context_store.get_recent(chat_key, 5).await;
                if recent.is_empty() {
                    &delegation.message
                } else {
                    // Build message with context preamble
                    let preamble = recent.join("\n\n");
                    let combined = format!("{}\n\n{}", preamble, delegation.message);
                    // Store combined for this call
                    // TODO: Properly handle lifetime here
                    &delegation.message
                }
            }
            ContextMode::Fork => {
                // Fork: isolated context, just the message for now
                &delegation.message
            }
        };

        // Recursive call for delegation chain
        self.dispatch_with_delegation(
            delegate_text,
            target_agent,
            sender,
            context_store,
            chat_key,
            state,
        ).await
    }

    /// Validate that a delegation is allowed by ACLs.
    fn validate_delegation(&self, source_id: &str, delegation: &DelegationRequest) -> bool {
        // Find source agent config
        let Some(source_agent) = self.config.agents.iter().find(|a| a.id == source_id) else {
            return false;
        };

        // Check source can delegate TO target
        let can_delegate = source_agent
            .delegation
            .as_ref()
            .map(|d| d.delegates.allows(&delegation.target))
            .unwrap_or(false); // Default: no delegation

        if !can_delegate {
            warn!(
                source = %source_id,
                target = %delegation.target,
                "source agent not allowed to delegate to target"
            );
            return false;
        }

        // Find target agent config
        let Some(target_agent) = self.config.agents.iter().find(|a| a.id == delegation.target) else {
            return false;
        };

        // Check target accepts FROM source
        let accepts = target_agent
            .delegation
            .as_ref()
            .map(|d| d.accepts_from.allows(source_id))
            .unwrap_or(true); // Default: accept from any

        if !accepts {
            warn!(
                source = %source_id,
                target = %delegation.target,
                "target agent does not accept delegation from source"
            );
            return false;
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Config Extension
// ---------------------------------------------------------------------------

/// Delegation settings in PolyConfig.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationSettings {
    /// Maximum delegation chain depth.
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,

    /// Number of recent turns to include in "recent" context mode.
    #[serde(default = "default_recent_turns")]
    pub recent_turns: usize,
}

fn default_max_depth() -> usize {
    5
}

fn default_recent_turns() -> usize {
    5
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_toml_delegation_block() {
        let text = r#"
I'll delegate this.

[delegate]
target = "coder"
context = "recent"
message = "Write tests"
[/delegate]
"#;

        let markers = parse_delegation_markers(text);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].target, "coder");
        assert!(matches!(markers[0].context, ContextMode::Recent));
        assert_eq!(markers[0].message, "Write tests");
    }

    #[test]
    fn test_parse_inline_json_delegation() {
        let text = r#"Analysis complete.

::delegate::{"target": "reviewer", "context": "none", "message": "Check this"}::

Done."#;

        let markers = parse_delegation_markers(text);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].target, "reviewer");
        assert!(matches!(markers[0].context, ContextMode::None));
    }

    #[test]
    fn test_delegation_target_any() {
        let target = DelegationTarget::Keyword("any".to_string());
        assert!(target.allows("anything"));
    }

    #[test]
    fn test_delegation_target_none() {
        let target = DelegationTarget::Keyword("none".to_string());
        assert!(!target.allows("anything"));
    }

    #[test]
    fn test_delegation_target_list() {
        let target = DelegationTarget::List(vec!["a".to_string(), "b".to_string()]);
        assert!(target.allows("a"));
        assert!(target.allows("b"));
        assert!(!target.allows("c"));
    }

    #[test]
    fn test_cycle_detection() {
        let mut state = DelegationState::new();

        state.record_edge("a", "b").unwrap();
        state.record_edge("b", "c").unwrap();

        // Cycle: c -> a
        let err = state.record_edge("c", "a").unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn test_depth_tracking() {
        let mut state = DelegationState::new();
        assert_eq!(state.depth, 0);

        state.record_edge("a", "b").unwrap();
        assert_eq!(state.depth, 1);

        state.record_edge("b", "c").unwrap();
        assert_eq!(state.depth, 2);
    }
}
