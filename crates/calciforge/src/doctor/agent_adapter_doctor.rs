//! First-class adapter doctor hooks.
//!
//! Generic config, endpoint, and subprocess checks still live in `doctor.rs`.
//! This module is the adapter-owned extension point for deeper checks that
//! depend on a specific downstream protocol or native runtime.

use crate::agent_kinds::{AgentKind, parse_agent_kind};
use crate::config::{AgentConfig, CalciforgeConfig};

use super::{DoctorReport, check_openclaw_channel_route, security_requires_agent_egress_proxy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdapterDoctorHook {
    /// Adapter has no protocol-specific doctor check yet; generic checks apply.
    NoOp,
    /// OpenClaw channel plugin status endpoint and callback/proxy validation.
    OpenClawChannel,
}

pub(super) async fn check(
    agent: &AgentConfig,
    config: &CalciforgeConfig,
    no_network: bool,
    report: &mut DoctorReport,
) {
    let Some(kind) = parse_agent_kind(&agent.kind) else {
        return;
    };

    match hook_for_kind(kind) {
        AdapterDoctorHook::NoOp => {}
        AdapterDoctorHook::OpenClawChannel => {
            if no_network {
                report.ok(format!(
                    "agent '{}' openclaw-channel native route check skipped by --no-network",
                    agent.id
                ));
                return;
            }
            check_openclaw_channel_route(
                agent,
                security_requires_agent_egress_proxy(config),
                report,
            )
            .await;
        }
    }
}

fn hook_for_kind(kind: AgentKind) -> AdapterDoctorHook {
    match kind {
        AgentKind::OpenClawChannel => AdapterDoctorHook::OpenClawChannel,
        AgentKind::OpenAiCompat
        | AgentKind::ZeroClawHttp
        | AgentKind::ZeroClawNative
        | AgentKind::ZeroClaw
        | AgentKind::IronClaw
        | AgentKind::Hermes
        | AgentKind::Exec
        | AgentKind::Cli
        | AgentKind::ArtifactCli
        | AgentKind::CodexCli
        | AgentKind::ClaudeCli
        | AgentKind::DiracCli
        | AgentKind::KimiCli
        | AgentKind::Acp
        | AgentKind::Acpx => AdapterDoctorHook::NoOp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kinds::ALL_AGENT_KINDS;

    #[test]
    fn every_known_adapter_kind_has_an_explicit_doctor_hook() {
        for kind in ALL_AGENT_KINDS {
            let _ = hook_for_kind(*kind);
        }
    }

    #[test]
    fn openclaw_channel_owns_the_native_route_hook() {
        assert_eq!(
            hook_for_kind(AgentKind::OpenClawChannel),
            AdapterDoctorHook::OpenClawChannel
        );
    }

    #[test]
    fn unimplemented_native_hooks_are_explicit_noops() {
        assert_eq!(hook_for_kind(AgentKind::ZeroClaw), AdapterDoctorHook::NoOp);
        assert_eq!(hook_for_kind(AgentKind::CodexCli), AdapterDoctorHook::NoOp);
        assert_eq!(hook_for_kind(AgentKind::Hermes), AdapterDoctorHook::NoOp);
    }
}
