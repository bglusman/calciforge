//! Shared migration types for the ZeroClawed installer.
//!
//! These types mirror the canonical definitions in
//! `crates/nonzeroclaw/src/onboard/migration.rs`.  They are duplicated here
//! because `zeroclawed` does not (yet) depend on `nonzeroclaw` — doing so would
//! create an awkward crate dependency direction.
//!
//! # Canonical source
//!
//! The authoritative definitions live in `nonzeroclaw::onboard::migration`:
//! - `OpenClawInstallation`
//! - `DetectedChannel`
//! - `ChannelOwner`
//! - `ChannelAssignment`
//!
//! # TODO (follow-on)
//!
//! Extract these types (and the JSON5 parser) to a shared `claw-types` crate.
//! Both `nonzeroclaw` and `zeroclawed` should depend on it.  This eliminates the
//! duplication.  See `docs/opus-review.md` D1 for the full context.
//!
//! Until then: if you change either copy, update the other too.

use std::path::PathBuf;

/// Everything ZeroClawed's installer knows about an existing OpenClaw installation.
///
/// Constructed from the filesystem and the parsed `openclaw.json`.  Used
/// during install to decide which channels to configure, what version we're
/// talking to, and where the config file lives for patching.
///
/// Mirrors `nonzeroclaw::onboard::migration::OpenClawInstallation`.
#[derive(Debug, Clone)]
pub struct OpenClawInstallation {
    /// Path to `openclaw.json` on the remote host.
    pub config_path: PathBuf,
    /// Parsed JSON value of the entire config tree.
    pub config: serde_json::Value,
    /// Root of the OpenClaw data directory.
    pub openclaw_dir: PathBuf,
    /// Channels detected in the config.
    pub channels: Vec<DetectedChannel>,
    /// OpenClaw version string, if readable.
    pub version: Option<String>,
}

/// A communication channel detected in an OpenClaw config.
///
/// Mirrors `nonzeroclaw::onboard::migration::DetectedChannel`.
#[derive(Debug, Clone)]
pub struct DetectedChannel {
    /// Canonical lowercase name: `"telegram"`, `"signal"`, etc.
    pub name: String,
    /// Whether the channel appears to be enabled.
    pub enabled: bool,
    /// True if at least one credential field is non-empty.
    pub has_credentials: bool,
    /// The raw JSON object for this channel's config block.
    pub config_snippet: serde_json::Value,
}

/// Who should own a channel after ZeroClawed takes over routing.
///
/// Mirrors `nonzeroclaw::onboard::migration::ChannelOwner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOwner {
    /// ZeroClawed routes this channel to one of its configured claws.
    ZeroClawed,
    /// OpenClaw keeps it: nothing changes in either config.
    OpenClaw,
    /// Deferred / not decided.
    Unassigned,
}

impl std::fmt::Display for ChannelOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelOwner::ZeroClawed => write!(f, "ZeroClawed"),
            ChannelOwner::OpenClaw => write!(f, "OpenClaw"),
            ChannelOwner::Unassigned => write!(f, "Skip"),
        }
    }
}

/// The result of the channel assignment step for one channel.
///
/// Mirrors `nonzeroclaw::onboard::migration::ChannelAssignment`.
#[derive(Debug, Clone)]
pub struct ChannelAssignment {
    pub channel: DetectedChannel,
    pub owner: ChannelOwner,
    /// Which ZeroClawed claw handles this channel (if `owner == ZeroClawed`).
    pub assigned_claw: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_owner_display() {
        assert_eq!(ChannelOwner::ZeroClawed.to_string(), "ZeroClawed");
        assert_eq!(ChannelOwner::OpenClaw.to_string(), "OpenClaw");
        assert_eq!(ChannelOwner::Unassigned.to_string(), "Skip");
    }

    #[test]
    fn channel_assignment_fields() {
        let ch = DetectedChannel {
            name: "telegram".into(),
            enabled: true,
            has_credentials: true,
            config_snippet: serde_json::json!({"botToken": "tok"}),
        };
        let assignment = ChannelAssignment {
            channel: ch,
            owner: ChannelOwner::ZeroClawed,
            assigned_claw: Some("librarian".into()),
        };
        assert_eq!(assignment.owner, ChannelOwner::ZeroClawed);
        assert_eq!(assignment.assigned_claw.as_deref(), Some("librarian"));
        assert!(assignment.channel.has_credentials);
    }

    #[test]
    fn openclaw_installation_fields() {
        let install = OpenClawInstallation {
            config_path: PathBuf::from("/home/user/.openclaw/openclaw.json"),
            config: serde_json::json!({"version": "2026.3.13"}),
            openclaw_dir: PathBuf::from("/home/user/.openclaw"),
            channels: vec![],
            version: Some("2026.3.13".into()),
        };
        assert_eq!(install.version.as_deref(), Some("2026.3.13"));
        assert!(install.channels.is_empty());
    }
}
