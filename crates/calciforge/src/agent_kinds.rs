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

pub fn parse_agent_kind(kind: &str) -> Option<AgentKind> {
    AgentKind::parse(kind)
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
}
