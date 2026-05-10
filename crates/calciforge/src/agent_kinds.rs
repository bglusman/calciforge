//! Shared agent adapter kind classification.
//!
//! Keep this as the single source of truth for adapter kind names used by
//! config validation, doctor diagnostics, and routing support checks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    OpenClawChannel,
    OpenAiCompat,
    ZeroClawHttp,
    ZeroClawNative,
    ZeroClaw,
    IronClaw,
    Hermes,
    Exec,
    Cli,
    ArtifactCli,
    CodexCli,
    ClaudeCli,
    DiracCli,
    KimiCli,
    Acp,
    Acpx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKindLifecycle {
    /// Recommended for ordinary operator configuration.
    Stable,
    /// Kept for compatibility, but not preferred for new deployments.
    Legacy,
    /// Available for targeted use while the contract is still settling.
    Experimental,
}

impl AgentKindLifecycle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Legacy => "legacy",
            Self::Experimental => "experimental",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentKindMetadata {
    pub kind: AgentKind,
    pub name: &'static str,
    pub lifecycle: AgentKindLifecycle,
    pub summary: &'static str,
}

impl AgentKind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "openclaw-channel" => Some(Self::OpenClawChannel),
            "openai-compat" => Some(Self::OpenAiCompat),
            "zeroclaw-http" => Some(Self::ZeroClawHttp),
            "zeroclaw-native" => Some(Self::ZeroClawNative),
            "zeroclaw" => Some(Self::ZeroClaw),
            "ironclaw" => Some(Self::IronClaw),
            "hermes" => Some(Self::Hermes),
            "exec" => Some(Self::Exec),
            "cli" => Some(Self::Cli),
            "artifact-cli" => Some(Self::ArtifactCli),
            "codex-cli" => Some(Self::CodexCli),
            "claude-cli" => Some(Self::ClaudeCli),
            "dirac-cli" => Some(Self::DiracCli),
            "kimi-cli" => Some(Self::KimiCli),
            "acp" => Some(Self::Acp),
            "acpx" => Some(Self::Acpx),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenClawChannel => "openclaw-channel",
            Self::OpenAiCompat => "openai-compat",
            Self::ZeroClawHttp => "zeroclaw-http",
            Self::ZeroClawNative => "zeroclaw-native",
            Self::ZeroClaw => "zeroclaw",
            Self::IronClaw => "ironclaw",
            Self::Hermes => "hermes",
            Self::Exec => "exec",
            Self::Cli => "cli",
            Self::ArtifactCli => "artifact-cli",
            Self::CodexCli => "codex-cli",
            Self::ClaudeCli => "claude-cli",
            Self::DiracCli => "dirac-cli",
            Self::KimiCli => "kimi-cli",
            Self::Acp => "acp",
            Self::Acpx => "acpx",
        }
    }

    pub fn needs_endpoint(self) -> bool {
        matches!(
            self,
            Self::OpenClawChannel
                | Self::OpenAiCompat
                | Self::ZeroClawHttp
                | Self::ZeroClawNative
                | Self::ZeroClaw
                | Self::IronClaw
                | Self::Hermes
        )
    }

    pub fn is_http_agent(self) -> bool {
        self.needs_endpoint()
    }

    pub fn is_subprocess_agent(self) -> bool {
        matches!(
            self,
            Self::Exec
                | Self::Cli
                | Self::ArtifactCli
                | Self::CodexCli
                | Self::ClaudeCli
                | Self::DiracCli
                | Self::KimiCli
                | Self::Acp
                | Self::Acpx
        )
    }
}

pub const AGENT_KIND_METADATA: &[AgentKindMetadata] = &[
    AgentKindMetadata {
        kind: AgentKind::OpenClawChannel,
        name: "openclaw-channel",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "OpenClaw bridge plugin with callback replies and native lane commands",
    },
    AgentKindMetadata {
        kind: AgentKind::OpenAiCompat,
        name: "openai-compat",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "OpenAI-compatible /v1/chat/completions endpoint",
    },
    AgentKindMetadata {
        kind: AgentKind::Hermes,
        name: "hermes",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Hermes HTTP API with session continuity",
    },
    AgentKindMetadata {
        kind: AgentKind::IronClaw,
        name: "ironclaw",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "IronClaw HTTP webhook adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::Cli,
        name: "cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Generic one-shot subprocess adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::ArtifactCli,
        name: "artifact-cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Subprocess adapter with a Calciforge-controlled artifact directory",
    },
    AgentKindMetadata {
        kind: AgentKind::CodexCli,
        name: "codex-cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Codex CLI adapter with optional named session continuity",
    },
    AgentKindMetadata {
        kind: AgentKind::ClaudeCli,
        name: "claude-cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Claude Code CLI adapter with optional session id",
    },
    AgentKindMetadata {
        kind: AgentKind::DiracCli,
        name: "dirac-cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Dirac JSON event CLI adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::KimiCli,
        name: "kimi-cli",
        lifecycle: AgentKindLifecycle::Stable,
        summary: "Kimi Code print-mode CLI adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::ZeroClaw,
        name: "zeroclaw",
        lifecycle: AgentKindLifecycle::Legacy,
        summary: "Direct ZeroClaw endpoint retained for compatibility",
    },
    AgentKindMetadata {
        kind: AgentKind::ZeroClawHttp,
        name: "zeroclaw-http",
        lifecycle: AgentKindLifecycle::Legacy,
        summary: "Older ZeroClaw-compatible webhook adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::Exec,
        name: "exec",
        lifecycle: AgentKindLifecycle::Legacy,
        summary: "Legacy alias for the generic CLI adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::ZeroClawNative,
        name: "zeroclaw-native",
        lifecycle: AgentKindLifecycle::Experimental,
        summary: "ZeroClaw-compatible adapter with in-process conversation history",
    },
    AgentKindMetadata {
        kind: AgentKind::Acp,
        name: "acp",
        lifecycle: AgentKindLifecycle::Experimental,
        summary: "Persistent stdio ACP adapter",
    },
    AgentKindMetadata {
        kind: AgentKind::Acpx,
        name: "acpx",
        lifecycle: AgentKindLifecycle::Experimental,
        summary: "acpx CLI adapter with discoverable sessions",
    },
];

pub fn parse_agent_kind(kind: &str) -> Option<AgentKind> {
    AgentKind::parse(kind)
}

pub fn agent_kind_metadata(kind: &str) -> Option<AgentKindMetadata> {
    AGENT_KIND_METADATA
        .iter()
        .copied()
        .find(|metadata| metadata.name == kind)
}

pub fn known_agent_kind_names() -> impl Iterator<Item = &'static str> {
    AGENT_KIND_METADATA
        .iter()
        .map(|metadata| metadata.kind.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agent_kinds_cover_first_class_http_adapters() {
        assert!(matches!(
            parse_agent_kind("openclaw-channel"),
            Some(AgentKind::OpenClawChannel)
        ));
        assert!(matches!(
            parse_agent_kind("ironclaw"),
            Some(AgentKind::IronClaw)
        ));
        assert!(matches!(
            parse_agent_kind("hermes"),
            Some(AgentKind::Hermes)
        ));
        assert!(parse_agent_kind("openclaw-http").is_none());
    }

    #[test]
    fn agent_kind_metadata_covers_all_parseable_kinds() {
        for metadata in AGENT_KIND_METADATA {
            assert_eq!(parse_agent_kind(metadata.name), Some(metadata.kind));
            assert_eq!(metadata.kind.as_str(), metadata.name);
            assert!(!metadata.summary.trim().is_empty());
        }
    }

    #[test]
    fn agent_kind_lifecycle_marks_legacy_and_experimental_paths() {
        assert_eq!(
            agent_kind_metadata("openclaw-channel").unwrap().lifecycle,
            AgentKindLifecycle::Stable
        );
        assert_eq!(
            agent_kind_metadata("zeroclaw").unwrap().lifecycle,
            AgentKindLifecycle::Legacy
        );
        assert_eq!(
            agent_kind_metadata("acp").unwrap().lifecycle,
            AgentKindLifecycle::Experimental
        );
    }
}
