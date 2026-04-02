//! Auth boundary — identity resolution and allow_list enforcement.
//!
//! This is the FIRST check on any inbound message. If the sender cannot be
//! resolved to a known identity, the message is silently dropped.

use crate::config::{Identity, PolyConfig};

/// The resolved identity of a sender, if authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    pub id: String,
    pub role: Option<String>,
}

/// Resolve a Telegram sender (numeric user ID) to a PolyConfig identity.
///
/// Returns `Some(ResolvedIdentity)` if the sender matches a known alias,
/// or `None` if the sender is unknown (message should be dropped).
pub fn resolve_telegram_sender(user_id: i64, config: &PolyConfig) -> Option<ResolvedIdentity> {
    let id_str = user_id.to_string();
    resolve_channel_sender("telegram", &id_str, config)
}

/// Generic channel sender resolution.
///
/// Looks through all identities for one whose aliases include
/// `{ channel: channel_kind, id: sender_id }`.
pub fn resolve_channel_sender(
    channel_kind: &str,
    sender_id: &str,
    config: &PolyConfig,
) -> Option<ResolvedIdentity> {
    for identity in &config.identities {
        for alias in &identity.aliases {
            if alias.channel == channel_kind && alias.id == sender_id {
                return Some(ResolvedIdentity {
                    id: identity.id.clone(),
                    role: identity.role.clone(),
                });
            }
        }
    }
    None
}

/// Look up the default agent for an identity.
///
/// Returns the agent ID string if a routing rule exists for this identity,
/// or `None` if no routing rule is configured.
pub fn default_agent_for(identity_id: &str, config: &PolyConfig) -> Option<String> {
    config
        .routing
        .iter()
        .find(|r| r.identity == identity_id)
        .map(|r| r.default_agent.clone())
}

/// Look up an agent config by ID.
pub fn find_agent<'a>(
    agent_id: &str,
    config: &'a PolyConfig,
) -> Option<&'a crate::config::AgentConfig> {
    config.agents.iter().find(|a| a.id == agent_id)
}

/// Check whether an identity is allowed to use a specific agent.
///
/// Returns `true` if:
/// - The routing rule has no `allowed_agents` restriction (empty list = unrestricted), OR
/// - The agent is in the `allowed_agents` list.
pub fn is_agent_allowed(identity_id: &str, agent_id: &str, config: &PolyConfig) -> bool {
    match config.routing.iter().find(|r| r.identity == identity_id) {
        Some(rule) => {
            rule.allowed_agents.is_empty() || rule.allowed_agents.iter().any(|a| a == agent_id)
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, ChannelAlias, Identity, PolyConfig, PolyHeader, RoutingRule};

    fn make_config() -> PolyConfig {
        PolyConfig {
            polyclaw: PolyHeader { version: 2 },
            identities: vec![
                Identity {
                    id: "brian".to_string(),
                    display_name: Some("Brian".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "8465871195".to_string(),
                    }],
                    role: Some("owner".to_string()),
                },
                Identity {
                    id: "david".to_string(),
                    display_name: Some("David".to_string()),
                    aliases: vec![ChannelAlias {
                        channel: "telegram".to_string(),
                        id: "15555550002".to_string(),
                    }],
                    role: Some("user".to_string()),
                },
            ],
            agents: vec![AgentConfig {
                id: "librarian".to_string(),
                kind: "openclaw-http".to_string(),
                endpoint: "http://10.0.0.20:18789".to_string(),
                timeout_ms: Some(120000),
                model: None,
                auth_token: None,
                api_key: None,
                command: None,
                args: None,
                env: None,
                registry: None,
                aliases: vec![],
            openclaw_agent_id: None,
            reply_port: None,
            reply_auth_token: None,
            }],
            routing: vec![
                RoutingRule {
                    identity: "brian".to_string(),
                    default_agent: "librarian".to_string(),
                    allowed_agents: vec![],
                },
                RoutingRule {
                    identity: "david".to_string(),
                    default_agent: "librarian".to_string(),
                    allowed_agents: vec!["librarian".to_string()],
                },
            ],
            channels: vec![],
            permissions: None,
            memory: None,
            context: Default::default(),
        }
    }

    #[test]
    fn test_resolve_known_telegram_sender() {
        let cfg = make_config();
        let identity = resolve_telegram_sender(8465871195, &cfg);
        assert!(identity.is_some());
        let id = identity.unwrap();
        assert_eq!(id.id, "brian");
        assert_eq!(id.role, Some("owner".to_string()));
    }

    #[test]
    fn test_resolve_unknown_telegram_sender_drops() {
        let cfg = make_config();
        let identity = resolve_telegram_sender(9999999999, &cfg);
        assert!(
            identity.is_none(),
            "unknown sender must return None (drop message)"
        );
    }

    #[test]
    fn test_resolve_second_identity() {
        let cfg = make_config();
        let identity = resolve_telegram_sender(15555550002, &cfg);
        assert!(identity.is_some());
        assert_eq!(identity.unwrap().id, "david");
    }

    #[test]
    fn test_resolve_channel_sender_generic() {
        let cfg = make_config();
        let identity = resolve_channel_sender("telegram", "8465871195", &cfg);
        assert!(identity.is_some());
        assert_eq!(identity.unwrap().id, "brian");
    }

    #[test]
    fn test_resolve_wrong_channel_drops() {
        let cfg = make_config();
        // Brian is only registered for telegram, not signal
        let identity = resolve_channel_sender("signal", "8465871195", &cfg);
        assert!(identity.is_none());
    }

    #[test]
    fn test_default_agent_for_known_identity() {
        let cfg = make_config();
        let agent = default_agent_for("brian", &cfg);
        assert_eq!(agent, Some("librarian".to_string()));
    }

    #[test]
    fn test_default_agent_for_unknown_identity() {
        let cfg = make_config();
        let agent = default_agent_for("unknown_person", &cfg);
        assert!(agent.is_none());
    }

    #[test]
    fn test_is_agent_allowed_unrestricted() {
        let cfg = make_config();
        // brian has no allowed_agents restriction (empty = unrestricted)
        assert!(is_agent_allowed("brian", "librarian", &cfg));
        assert!(is_agent_allowed("brian", "any_agent", &cfg));
    }

    #[test]
    fn test_is_agent_allowed_restricted() {
        let cfg = make_config();
        // david has allowed_agents = ["librarian"]
        assert!(is_agent_allowed("david", "librarian", &cfg));
        assert!(!is_agent_allowed("david", "custodian", &cfg));
    }

    #[test]
    fn test_is_agent_allowed_no_routing_rule() {
        let cfg = make_config();
        assert!(!is_agent_allowed("unknown_person", "librarian", &cfg));
    }

    #[test]
    fn test_find_agent_exists() {
        let cfg = make_config();
        let agent = find_agent("librarian", &cfg);
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().endpoint, "http://10.0.0.20:18789");
    }

    #[test]
    fn test_find_agent_missing() {
        let cfg = make_config();
        let agent = find_agent("nonexistent", &cfg);
        assert!(agent.is_none());
    }
}
