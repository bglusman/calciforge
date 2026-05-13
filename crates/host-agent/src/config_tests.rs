use super::*;

#[test]
fn test_requires_approval() {
    let config = Config::default();

    // zfs-destroy requires approval
    assert!(config.requires_approval("zfs-destroy", "tank/media"));

    // zfs-list does not
    assert!(!config.requires_approval("zfs-list", "tank/media"));
}

#[test]
fn test_find_agent() {
    let config = Config::default();

    // Should find the generic Calciforge client config
    let agent = config.find_agent("calciforge-agent");
    assert!(agent.is_some());
    assert_eq!(agent.unwrap().agent_type, "generic");

    // Should find claude-code-main via wildcard
    let agent = config.find_agent("claude-code-main");
    assert!(agent.is_some());

    // Should not find unknown agent
    let agent = config.find_agent("unknown-agent");
    assert!(agent.is_none());
}

#[test]
fn test_find_agent_rejects_global_wildcard_pattern() {
    let config = Config {
        agents: vec![AgentConfig {
            cn_pattern: "*".to_string(),
            agent_type: "generic".to_string(),
            unix_user: "clash-agent".to_string(),
            autonomy: AutonomyLevel::Supervised,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: false,
        }],
        ..Default::default()
    };

    assert!(config.find_agent("any-agent").is_none());
    assert!(config.find_agent("").is_none());
}

#[test]
fn test_find_agent_rejects_empty_cn_pattern() {
    let config = Config {
        agents: vec![AgentConfig {
            cn_pattern: String::new(),
            agent_type: "generic".to_string(),
            unix_user: "clash-agent".to_string(),
            autonomy: AutonomyLevel::Supervised,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: false,
        }],
        ..Default::default()
    };

    assert!(config.find_agent("").is_none());
    assert!(config.find_agent("any-agent").is_none());
}

#[test]
fn test_autonomy_level_deserialize() {
    // AutonomyLevel is used as the value for the `autonomy` field in AgentConfig.
    // Test via a wrapper struct to mimic TOML deserialization.
    #[derive(serde::Deserialize)]
    struct Wrapper {
        autonomy: AutonomyLevel,
    }

    let w: Wrapper = toml::from_str(r#"autonomy = "supervised""#).unwrap();
    assert_eq!(w.autonomy, AutonomyLevel::Supervised);

    let w: Wrapper = toml::from_str(r#"autonomy = "full""#).unwrap();
    assert_eq!(w.autonomy, AutonomyLevel::Full);

    let w: Wrapper = toml::from_str(r#"autonomy = "read_only""#).unwrap();
    assert_eq!(w.autonomy, AutonomyLevel::ReadOnly);
}

/// P-B4: Full autonomy cannot bypass always_ask = true operations (default safe)
#[test]
fn test_full_autonomy_cannot_bypass_always_ask() {
    let config = Config::default();

    // zfs-destroy has always_ask = true in default rules
    let full_agent = AgentConfig {
        cn_pattern: "full-agent*".to_string(),
        agent_type: "test".to_string(),
        unix_user: "test".to_string(),
        autonomy: AutonomyLevel::Full,
        allowed_operations: vec![],
        requires_approval_for: vec![],
        pattern_rules: vec![],
        allow_full_autonomy_bypass: true, // even with bypass enabled
    };

    // always_ask=true overrides even allow_full_autonomy_bypass=true
    assert!(
        config.requires_approval_for_agent("zfs-destroy", "tank/media", Some(&full_agent)),
        "Full autonomy with bypass should NOT bypass always_ask=true operations"
    );
}

/// P-B4: Full autonomy CAN bypass when always_ask=false and bypass is explicitly enabled
#[test]
fn test_full_autonomy_bypass_when_explicitly_enabled() {
    let mut config = Config::default();
    // Set snapshot rule: approval required, but always_ask=false
    config.rules.push(RuleConfig {
        operation: "zfs-snapshot-protected".to_string(),
        approval_required: true,
        pattern: None,
        always_ask: false,
        approval_admin_only: false,
    });

    let full_agent_with_bypass = AgentConfig {
        cn_pattern: "full-agent*".to_string(),
        agent_type: "test".to_string(),
        unix_user: "test".to_string(),
        autonomy: AutonomyLevel::Full,
        allowed_operations: vec![],
        requires_approval_for: vec![],
        pattern_rules: vec![],
        allow_full_autonomy_bypass: true,
    };

    let full_agent_no_bypass = AgentConfig {
        allow_full_autonomy_bypass: false,
        ..full_agent_with_bypass.clone()
    };

    // With bypass enabled: skip approval
    assert!(
        !config.requires_approval_for_agent(
            "zfs-snapshot-protected",
            "tank/data",
            Some(&full_agent_with_bypass)
        ),
        "Full autonomy with bypass=true should skip approval when always_ask=false"
    );

    // Without bypass: still require approval
    assert!(
        config.requires_approval_for_agent(
            "zfs-snapshot-protected",
            "tank/data",
            Some(&full_agent_no_bypass)
        ),
        "Full autonomy with bypass=false should still require approval"
    );
}

/// P-B4: Supervised autonomy always requires approval regardless of bypass flag
#[test]
fn test_supervised_always_requires_approval() {
    let config = Config::default();

    let supervised_agent = AgentConfig {
        cn_pattern: "supervised*".to_string(),
        agent_type: "test".to_string(),
        unix_user: "test".to_string(),
        autonomy: AutonomyLevel::Supervised,
        allowed_operations: vec![],
        requires_approval_for: vec![],
        pattern_rules: vec![],
        allow_full_autonomy_bypass: true, // irrelevant for Supervised
    };

    assert!(
        config.requires_approval_for_agent("zfs-destroy", "tank/media", Some(&supervised_agent)),
        "Supervised agent should require approval"
    );
}
