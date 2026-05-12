use super::*;

use crate::config::{
    CalciforgeHeader, ProxyConfig, RoutingRule, SecuritySectionConfig, SyntheticModelConfig,
};
use std::collections::HashMap;

fn base_config() -> CalciforgeConfig {
    CalciforgeConfig {
        calciforge: CalciforgeHeader { version: 2 },
        agents: vec![AgentConfig {
            id: "gateway".to_string(),
            kind: "openclaw-channel".to_string(),
            endpoint: "http://127.0.0.1:18083".to_string(),
            ..Default::default()
        }],
        routing: vec![RoutingRule {
            identity: "brian".to_string(),
            default_agent: "gateway".to_string(),
            btw_agent: None,
            allowed_agents: vec!["gateway".to_string()],
        }],
        proxy: Some(ProxyConfig {
            enabled: true,
            bind: "127.0.0.1:18083".to_string(),
            ..Default::default()
        }),
        dispatchers: vec![crate::config::DispatcherConfig {
            id: "local-kimi-gpt55".to_string(),
            name: Some("Local Kimi then GPT".to_string()),
            models: vec![SyntheticModelConfig {
                model: "kimi-for-coding".to_string(),
                context_window: 128_000,
            }],
        }],
        identities: vec![],
        channels: vec![],
        permissions: None,
        memory: None,
        context: Default::default(),
        model_shortcuts: vec![],
        model_roles: vec![],
        alloys: vec![],
        cascades: vec![],
        exec_models: vec![],
        security: None,
        local_models: None,
    }
}

#[test]
fn subprocess_agent_proxy_coverage_accepts_missing_proxy_env() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "codex".to_string(),
        kind: "codex-cli".to_string(),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: None,
            https: None,
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Ok
            && finding.message.contains("have no explicit MITM proxy env")
    }));
}

#[test]
fn install_override_requires_agent_egress_proxy() {
    let config = base_config();

    assert!(!security_requires_agent_egress_proxy_with_override(
        &config, None
    ));
    assert!(security_requires_agent_egress_proxy_with_override(
        &config,
        Some("1")
    ));
    assert!(security_requires_agent_egress_proxy_with_override(
        &config,
        Some("yes")
    ));
    assert!(!security_requires_agent_egress_proxy_with_override(
        &config,
        Some("false")
    ));
}

#[test]
fn explicit_doctor_override_can_only_make_egress_proxy_stricter() {
    let mut config = base_config();

    assert!(!effective_agent_egress_proxy_requirement(&config, None));
    assert!(effective_agent_egress_proxy_requirement(
        &config,
        Some(true)
    ));
    assert!(!effective_agent_egress_proxy_requirement(
        &config,
        Some(false)
    ));

    config.security = Some(SecuritySectionConfig {
        profile: "hardened".to_string(),
        scan_outbound: Some(true),
        require_agent_egress_proxy: true,
        scanner_checks: vec![],
    });
    assert!(effective_agent_egress_proxy_requirement(
        &config,
        Some(false)
    ));
}

#[test]
fn explicit_doctor_override_does_not_error_on_external_daemon_unverifiability() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "openclaw".to_string(),
        kind: "openclaw-channel".to_string(),
        endpoint: "http://127.0.0.1:18789".to_string(),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage_with_strict(
        &config,
        &ProxyEnvironment::default(),
        true,
        false,
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Warn
            && finding
                .message
                .contains("doctor cannot verify their process proxy environment")
    }));
    assert!(!report.findings.iter().any(|finding| {
        finding.severity == Severity::Error
            && finding
                .message
                .contains("doctor cannot verify their process proxy environment")
    }));
}

#[test]
fn subprocess_agent_proxy_coverage_warns_on_complete_agent_proxy_env() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "dirac".to_string(),
        kind: "dirac-cli".to_string(),
        env: Some(HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8888".to_string(),
            ),
            (
                "HTTPS_PROXY".to_string(),
                "http://127.0.0.1:8888".to_string(),
            ),
            ("ALL_PROXY".to_string(), "http://127.0.0.1:8888".to_string()),
            (
                "NO_PROXY".to_string(),
                "localhost,127.0.0.1,::1".to_string(),
            ),
            (
                "NODE_EXTRA_CA_CERTS".to_string(),
                "/tmp/mitm-ca.pem".to_string(),
            ),
        ])),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: None,
            https: None,
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Warn
            && finding.message.contains("define complete MITM proxy env")
    }));
}

#[test]
fn subprocess_agent_proxy_coverage_warns_when_agent_env_is_incomplete() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "codex".to_string(),
        kind: "codex-cli".to_string(),
        env: Some(HashMap::from([(
            "https_proxy".to_string(),
            "http://127.0.0.1:9999".to_string(),
        )])),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: None,
            https: None,
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Warn
            && finding.message.contains("define incomplete MITM proxy env")
    }));
}

#[test]
fn subprocess_agent_proxy_coverage_errors_in_strict_security_without_proxy_env() {
    let mut config = base_config();
    config.security = Some(SecuritySectionConfig {
        profile: "hardened".to_string(),
        scan_outbound: Some(true),
        require_agent_egress_proxy: true,
        scanner_checks: vec![],
    });
    config.agents = vec![AgentConfig {
        id: "codex".to_string(),
        kind: "codex-cli".to_string(),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Error
            && finding.message.contains("have no explicit MITM proxy env")
    }));
}

#[test]
fn scan_outbound_false_relaxes_profile_default_strict_egress() {
    let mut config = base_config();
    config.security = Some(SecuritySectionConfig {
        profile: "hardened".to_string(),
        scan_outbound: Some(false),
        require_agent_egress_proxy: false,
        scanner_checks: vec![],
    });

    assert!(!security_requires_agent_egress_proxy(&config));
}

#[test]
fn subprocess_agent_proxy_coverage_warns_when_agent_env_clears_proxy() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "codex".to_string(),
        kind: "codex-cli".to_string(),
        env: Some(HashMap::from([("HTTP_PROXY".to_string(), String::new())])),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: None,
            https: None,
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Warn && finding.message.contains("set empty proxy env values")
    }));
}

#[test]
fn external_agent_proxy_coverage_errors_in_strict_security() {
    let mut config = base_config();
    config.security = Some(SecuritySectionConfig {
        profile: "hardened".to_string(),
        scan_outbound: Some(true),
        require_agent_egress_proxy: true,
        scanner_checks: vec![],
    });
    config.agents = vec![AgentConfig {
        id: "openclaw".to_string(),
        kind: "openclaw-channel".to_string(),
        endpoint: "http://127.0.0.1:18789".to_string(),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: Some("http://127.0.0.1:8888".to_string()),
            https: Some("http://127.0.0.1:8888".to_string()),
            all: Some("http://127.0.0.1:8888".to_string()),
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            node_extra_ca_certs: Some("/tmp/mitm-ca.pem".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Error
            && finding
                .message
                .contains("doctor cannot verify their process proxy environment")
    }));
}

#[test]
fn external_agent_proxy_coverage_warns_that_daemon_env_is_unverified() {
    let mut config = base_config();
    config.agents = vec![AgentConfig {
        id: "openclaw".to_string(),
        kind: "openclaw-channel".to_string(),
        endpoint: "http://127.0.0.1:18789".to_string(),
        ..Default::default()
    }];
    let mut report = DoctorReport::default();

    check_agent_proxy_coverage(
        &config,
        &ProxyEnvironment {
            http: Some("http://127.0.0.1:8888".to_string()),
            https: Some("http://127.0.0.1:8888".to_string()),
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            ..Default::default()
        },
        &mut report,
    );

    assert!(report.findings.iter().any(|finding| {
        finding.severity == Severity::Warn
            && finding
                .message
                .contains("doctor cannot verify their process proxy environment")
    }));
}
