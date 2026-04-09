//! Core data model for the ZeroClawed multi-target installer.
//!
//! The key design axis is **remote configurability**:
//! - [`ClawKind::NzcNative`] and [`ClawKind::OpenClawHttp`] → installer
//!   knows the config format and can SSH in to make changes safely.
//! - All other adapters → installer just registers the endpoint in ZeroClawed's
//!   config and health-checks it; no SSH config management is performed.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ClawKind
// ---------------------------------------------------------------------------

/// How ZeroClawed dispatches messages to a downstream claw, and whether the
/// installer can manage its remote config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClawKind {
    /// NonZeroClaw native webhook protocol.
    /// Installer can SSH in and edit NZC's config.
    NzcNative,

    /// OpenClaw OpenAI-compatible HTTP gateway.
    /// Installer can SSH in and safely edit `openclaw.json`.
    OpenClawHttp,

    /// Any OpenAI-compatible `/v1/chat/completions` endpoint.
    /// Installer registers the endpoint only — no SSH config management.
    OpenAiCompat { endpoint: String },

    /// Generic HTTP webhook receiver.
    /// Installer registers the endpoint only — no SSH config management.
    Webhook {
        endpoint: String,
        format: WebhookFormat,
    },

    /// Spawn a local binary and read its stdout.
    /// No network health-check possible; installer records command only.
    Cli { command: String },
}

impl ClawKind {
    /// Returns `true` if this adapter supports remote SSH config management.
    ///
    /// Only [`ClawKind::NzcNative`] and [`ClawKind::OpenClawHttp`] do.
    /// All other adapters are registered in ZeroClawed's config and health-checked,
    /// but the installer never SSHes into them to edit their config files.
    pub fn is_remotely_configurable(&self) -> bool {
        matches!(self, ClawKind::NzcNative | ClawKind::OpenClawHttp)
    }

    /// Short label for display and TOML serialization.
    pub fn kind_label(&self) -> &'static str {
        match self {
            ClawKind::NzcNative => "nzc",
            ClawKind::OpenClawHttp => "openclaw",
            ClawKind::OpenAiCompat { .. } => "openai-compat",
            ClawKind::Webhook { .. } => "webhook",
            ClawKind::Cli { .. } => "cli",
        }
    }
}

// ---------------------------------------------------------------------------
// WebhookFormat
// ---------------------------------------------------------------------------

/// Wire format for [`ClawKind::Webhook`] POST requests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WebhookFormat {
    /// POST `{"message": "<text>"}` JSON body.
    #[default]
    Json,
    /// POST raw text body.
    Text,
}

impl std::fmt::Display for WebhookFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebhookFormat::Json => write!(f, "json"),
            WebhookFormat::Text => write!(f, "text"),
        }
    }
}

// ---------------------------------------------------------------------------
// ClawTarget
// ---------------------------------------------------------------------------

/// A downstream claw that ZeroClawed will route messages to.
///
/// `host` is used for SSH operations (when `adapter.is_remotely_configurable()`)
/// and for display purposes.  For adapters that don't require SSH, `host` may
/// be empty or just the hostname portion of `endpoint`.
///
/// `ssh_key` is only needed when `adapter.is_remotely_configurable()`.
#[derive(Debug, Clone)]
pub struct ClawTarget {
    /// Friendly name used in ZeroClawed config and logs (e.g. `"librarian"`).
    pub name: String,
    /// How ZeroClawed dispatches to this claw and whether remote config is possible.
    pub adapter: ClawKind,
    /// SSH target for remote config operations: `user@hostname` or `hostname`.
    /// Required when `adapter.is_remotely_configurable()`, optional otherwise.
    pub host: String,
    /// Path to SSH private key.
    /// Required when `adapter.is_remotely_configurable()`, `None` otherwise.
    pub ssh_key: Option<PathBuf>,
    /// HTTP endpoint ZeroClawed sends messages to (empty for `Cli` adapters).
    pub endpoint: String,
}

impl ClawTarget {
    /// Returns `true` if this target will involve SSH remote config management.
    pub fn needs_ssh_config(&self) -> bool {
        self.adapter.is_remotely_configurable()
    }

    /// Returns the SSH key path, or an error if this target needs SSH but has
    /// no key configured.
    pub fn ssh_key_required(&self) -> anyhow::Result<&PathBuf> {
        self.ssh_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "claw '{}' uses adapter '{}' which requires SSH, but no ssh_key was provided",
                self.name,
                self.adapter.kind_label(),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// ZeroClawedTarget
// ---------------------------------------------------------------------------

/// Where ZeroClawed itself is running (may be local or remote).
#[derive(Debug, Clone)]
pub struct ZeroClawedTarget {
    /// SSH target for ZeroClawed's host (`user@hostname`).
    pub host: String,
    /// SSH private key for ZeroClawed's host.
    pub ssh_key: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// InstallTarget
// ---------------------------------------------------------------------------

/// Top-level install descriptor: where ZeroClawed lives + all downstream claws.
#[derive(Debug, Clone)]
pub struct InstallTarget {
    /// ZeroClawed host metadata.
    pub zeroclawed: ZeroClawedTarget,
    /// Downstream claws to register and (optionally) configure.
    pub claws: Vec<ClawTarget>,
}

// ---------------------------------------------------------------------------
// Version compatibility
// ---------------------------------------------------------------------------

/// Known-compatible OpenClaw versions (semver-style year.month.patch).
///
/// This list is intentionally conservative: unknown versions get a warning,
/// not a hard stop (operator can override with `--yes`).
const OPENCLAW_COMPATIBLE_VERSIONS: &[&str] = &[
    "2026.3.13",
    "2026.3.14",
    "2026.3.15",
    "2026.3.0",
    "2026.2.0",
    "2026.1.0",
];

/// Known-compatible NZC versions.
const NZC_COMPATIBLE_VERSIONS: &[&str] = &["0.1.0", "0.2.0", "0.3.0", "0.4.0"];

/// Compatibility verdict for a detected version string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCompatibility {
    /// Explicitly known-compatible.
    Compatible,
    /// Not in the known list — warn but allow with `--yes`.
    Unknown,
    /// Explicitly known-incompatible (reserved for future use).
    Incompatible { reason: String },
}

impl std::fmt::Display for VersionCompatibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionCompatibility::Compatible => write!(f, "compatible"),
            VersionCompatibility::Unknown => write!(f, "unknown (proceed with caution)"),
            VersionCompatibility::Incompatible { reason } => {
                write!(f, "incompatible: {}", reason)
            }
        }
    }
}

/// Check a version string against the known compatibility list for a given adapter.
pub fn check_version_compatibility(adapter: &ClawKind, version: &str) -> VersionCompatibility {
    let compatible_list = match adapter {
        ClawKind::OpenClawHttp => OPENCLAW_COMPATIBLE_VERSIONS,
        ClawKind::NzcNative => NZC_COMPATIBLE_VERSIONS,
        // Non-SSH adapters: we don't manage their config, so version is informational only.
        _ => return VersionCompatibility::Compatible,
    };

    if compatible_list.contains(&version) {
        VersionCompatibility::Compatible
    } else {
        VersionCompatibility::Unknown
    }
}

// ---------------------------------------------------------------------------
// Backup filename generation
// ---------------------------------------------------------------------------

/// Generate a timestamped backup filename for a config file.
///
/// Format: `<original_path>.bak.<unix_timestamp_millis>`
///
/// Guaranteed unique for distinct millisecond instants; callers that need
/// additional uniqueness can append a random suffix.
pub fn backup_filename(original: &str, timestamp_millis: u64) -> String {
    format!("{}.bak.{}", original, timestamp_millis)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_incompatible_variant_display() {
        let v = VersionCompatibility::Incompatible {
            reason: "test".to_string(),
        };
        assert!(v.to_string().contains("incompatible"));
    }

    #[test]
    fn remotely_configurable_only_nzc_and_openclaw() {
        assert!(ClawKind::NzcNative.is_remotely_configurable());
        assert!(ClawKind::OpenClawHttp.is_remotely_configurable());
        assert!(!ClawKind::OpenAiCompat {
            endpoint: "http://localhost".into()
        }
        .is_remotely_configurable());
        assert!(!ClawKind::Webhook {
            endpoint: "http://localhost/hook".into(),
            format: WebhookFormat::Json,
        }
        .is_remotely_configurable());
        assert!(!ClawKind::Cli {
            command: "my-claw".into()
        }
        .is_remotely_configurable());
    }

    #[test]
    fn kind_label_roundtrip() {
        assert_eq!(ClawKind::NzcNative.kind_label(), "nzc");
        assert_eq!(ClawKind::OpenClawHttp.kind_label(), "openclaw");
        assert_eq!(
            ClawKind::OpenAiCompat {
                endpoint: "http://x".into()
            }
            .kind_label(),
            "openai-compat"
        );
        assert_eq!(
            ClawKind::Webhook {
                endpoint: "http://x".into(),
                format: WebhookFormat::Text,
            }
            .kind_label(),
            "webhook"
        );
        assert_eq!(
            ClawKind::Cli {
                command: "x".into()
            }
            .kind_label(),
            "cli"
        );
    }

    #[test]
    fn claw_target_needs_ssh_config() {
        let nzc = ClawTarget {
            name: "nzc".into(),
            adapter: ClawKind::NzcNative,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/key")),
            endpoint: "http://host:18799".into(),
        };
        assert!(nzc.needs_ssh_config());

        let openai = ClawTarget {
            name: "openai".into(),
            adapter: ClawKind::OpenAiCompat {
                endpoint: "http://host/v1".into(),
            },
            host: "host".into(),
            ssh_key: None,
            endpoint: "http://host/v1".into(),
        };
        assert!(!openai.needs_ssh_config());
    }

    #[test]
    fn ssh_key_required_returns_key_when_present() {
        let target = ClawTarget {
            name: "t".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_rsa")),
            endpoint: "http://host:18789".into(),
        };
        let key = target.ssh_key_required().unwrap();
        assert_eq!(key, &PathBuf::from("/keys/id_rsa"));
    }

    #[test]
    fn ssh_key_required_errors_when_missing() {
        let target = ClawTarget {
            name: "t".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: None,
            endpoint: "http://host:18789".into(),
        };
        let result = target.ssh_key_required();
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("ssh_key"),
            "error should mention ssh_key: {}",
            msg
        );
    }

    #[test]
    fn version_compat_known_openclaw() {
        assert_eq!(
            check_version_compatibility(&ClawKind::OpenClawHttp, "2026.3.13"),
            VersionCompatibility::Compatible
        );
        assert_eq!(
            check_version_compatibility(&ClawKind::OpenClawHttp, "9999.99.99"),
            VersionCompatibility::Unknown
        );
    }

    #[test]
    fn version_compat_known_nzc() {
        assert_eq!(
            check_version_compatibility(&ClawKind::NzcNative, "0.1.0"),
            VersionCompatibility::Compatible
        );
        assert_eq!(
            check_version_compatibility(&ClawKind::NzcNative, "99.0.0"),
            VersionCompatibility::Unknown
        );
    }

    #[test]
    fn version_compat_non_ssh_adapters_always_compatible() {
        // Non-SSH adapters: version is informational, never blocks
        assert_eq!(
            check_version_compatibility(
                &ClawKind::OpenAiCompat {
                    endpoint: "http://x".into()
                },
                "anything-at-all"
            ),
            VersionCompatibility::Compatible
        );
        assert_eq!(
            check_version_compatibility(
                &ClawKind::Webhook {
                    endpoint: "http://x".into(),
                    format: WebhookFormat::Json,
                },
                ""
            ),
            VersionCompatibility::Compatible
        );
        assert_eq!(
            check_version_compatibility(
                &ClawKind::Cli {
                    command: "cmd".into()
                },
                "1.0"
            ),
            VersionCompatibility::Compatible
        );
    }

    #[test]
    fn backup_filename_contains_path_and_timestamp() {
        let name = backup_filename("/etc/openclaw.json", 1_711_900_000_000);
        assert!(name.starts_with("/etc/openclaw.json"));
        assert!(name.contains("1711900000000"));
    }

    // ── Property tests ──────────────────────────────────────────────────────

    #[cfg(test)]
    mod prop_tests {
        use super::*;

        /// Property: backup_filename always starts with the original path.
        #[test]
        fn backup_filename_always_prefixed_by_original() {
            let cases: &[(&str, u64)] = &[
                ("/path/to/file.json", 0),
                ("/path/to/file.json", u64::MAX),
                ("relative/path.json", 1_000_000),
                ("", 42),
                ("/weird path/with spaces/file", 999),
            ];
            for (path, ts) in cases {
                let bak = backup_filename(path, *ts);
                assert!(
                    bak.starts_with(path),
                    "backup '{}' should start with original path '{}'",
                    bak,
                    path
                );
            }
        }

        /// Property: backup_filename with different timestamps always produces
        /// different filenames (for the same original path).
        #[test]
        fn backup_filename_unique_for_different_timestamps() {
            let path = "/etc/openclaw.json";
            let timestamps: &[u64] = &[0, 1, 1000, 1_711_900_000_000, u64::MAX - 1, u64::MAX];
            let names: Vec<String> = timestamps
                .iter()
                .map(|ts| backup_filename(path, *ts))
                .collect();

            // All names must be unique
            let unique_count = {
                let mut seen = std::collections::HashSet::new();
                for n in &names {
                    seen.insert(n.clone());
                }
                seen.len()
            };
            assert_eq!(
                unique_count,
                timestamps.len(),
                "different timestamps must produce different backup names"
            );
        }

        /// Property: backup_filename never equals the original path.
        #[test]
        fn backup_filename_never_equals_original() {
            let paths = &[
                "/etc/openclaw.json",
                "nzc.toml",
                "/home/user/.config/thing.json",
            ];
            let timestamps: &[u64] = &[0, 1, 9999999999];
            for path in paths {
                for ts in timestamps {
                    let bak = backup_filename(path, *ts);
                    assert_ne!(bak, *path, "backup filename must differ from original path");
                }
            }
        }
    }
}

/// Hegel property tests using the hegeltest crate (imported as `hegel`).
#[cfg(all(test, feature = "hegel"))]
mod hegel_tests {
    use super::*;
    use hegel::generators::*;
    use hegel::{Generator, TestCase};

    /// Property: backup_filename is always unique for distinct timestamps (arbitrary paths).
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_backup_unique_timestamps(tc: TestCase) {
        let path = tc.draw(text().max_size(80));
        let ts1 = tc.draw(integers::<u64>());
        // ts2 is ts1 + 1 (saturating), always different unless ts1 == u64::MAX
        let ts2 = ts1.saturating_add(1);
        if ts1 == ts2 {
            return; // edge case at u64::MAX — skip
        }
        let b1 = backup_filename(&path, ts1);
        let b2 = backup_filename(&path, ts2);
        assert_ne!(
            b1, b2,
            "distinct timestamps must produce distinct backup names"
        );
    }

    /// Property: backup_filename always contains the timestamp as a substring.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_backup_contains_timestamp(tc: TestCase) {
        let path = tc.draw(text().max_size(80));
        let ts = tc.draw(integers::<u64>());
        let bak = backup_filename(&path, ts);
        let ts_str = ts.to_string();
        assert!(
            bak.contains(&ts_str),
            "backup '{}' must contain timestamp '{}'",
            bak,
            ts_str
        );
    }

    /// Property: SSH command construction never injects shell metacharacters.
    ///
    /// Verifies that `shell_quote` always produces a correctly-structured
    /// single-quoted string:
    /// - Starts and ends with `'`
    /// - For inputs without single-quotes: inner content is exactly the input
    /// - For inputs with single-quotes: uses the `'\''` POSIX escape idiom
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_shell_quote_safe(tc: TestCase) {
        // Restrict to printable ASCII to model realistic paths/hosts.
        let s = tc.draw(
            text()
                .max_size(60)
                .filter(|s: &String| s.chars().all(|c| c.is_ascii_graphic() || c == ' ')),
        );
        let quoted = super::super::ssh::shell_quote(&s);
        // Always starts and ends with single-quote.
        assert!(quoted.starts_with('\''), "must start with ': {}", quoted);
        assert!(quoted.ends_with('\''), "must end with ': {}", quoted);
        // For strings WITHOUT single-quotes: inner must be exactly the input.
        if !s.contains('\'') {
            let inner = &quoted[1..quoted.len() - 1];
            assert_eq!(
                inner,
                s.as_str(),
                "inner of quoted string must equal input when no single-quotes present"
            );
        }
        // For strings WITH single-quotes: the escape idiom must be present.
        if s.contains('\'') {
            assert!(
                quoted.contains("'\\''"),
                "inputs with single-quotes must use '\\'' escape idiom; input={:?} quoted={}",
                s,
                quoted
            );
        }
    }

    /// Property: `backup_filename` always contains the original path's
    /// filename component (the last segment after `/`).
    ///
    /// From opus-review-2.md §7: the backup filename must be recoverable.
    /// If the original filename component is present as a prefix, you can
    /// extract the original path.
    ///
    /// Edge case the property detects: if the path contains `.bak.` already,
    /// the backup filename should still start with the full original path.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_backup_filename_contains_original_path(tc: TestCase) {
        let path = tc.draw(text().min_size(1).max_size(60).filter(|s: &String| {
            // Restrict to printable ASCII paths, no control characters.
            s.chars().all(|c| c.is_ascii() && c >= ' ' && c != '\x7f')
        }));
        let ts = tc.draw(integers::<u64>());
        let bak = backup_filename(&path, ts);

        // The backup filename must START with the original path.
        // Format is `<original_path>.bak.<timestamp>` — so the original path
        // is always recoverable as everything before `.bak.<ts>`.
        assert!(
            bak.starts_with(&path),
            "backup_filename must start with original path\n\
             path: {:?}\n\
             bak:  {:?}",
            path,
            bak
        );
    }

    /// Property: the backup filename roundtrip — original path is recoverable.
    ///
    /// For any (path, timestamp), splitting the backup filename at `.bak.`
    /// and taking the first part recovers the original path.  This fails if
    /// the format changes in a way that breaks recovery (e.g. double `.bak.`
    /// in paths with `.bak.` already in them).
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_backup_filename_roundtrip_recovery(tc: TestCase) {
        // Restrict path to content that is safe for the recovery split.
        // We test the general case first (paths without `.bak.`).
        let path = tc.draw(text().min_size(1).max_size(60).filter(|s: &String| {
            s.chars().all(|c| c.is_ascii() && c >= ' ' && c != '\x7f') && !s.contains(".bak.")
        }));
        let ts = tc.draw(integers::<u64>());
        let bak = backup_filename(&path, ts);

        // Recovery: split at the LAST `.bak.` to get original path.
        // The format is `<path>.bak.<ts>` so splitting on `.bak.` recovers path.
        let separator = ".bak.";
        let recovered_path = bak.rfind(separator).map(|idx| &bak[..idx]).unwrap_or(&bak);

        assert_eq!(
            recovered_path,
            path.as_str(),
            "backup roundtrip: original path not recoverable\n\
             path: {:?}\n\
             bak:  {:?}",
            path,
            bak
        );

        // Also verify the timestamp is recoverable.
        let recovered_ts_str = bak
            .rfind(separator)
            .map(|idx| &bak[idx + separator.len()..])
            .unwrap_or("");
        assert_eq!(
            recovered_ts_str,
            ts.to_string().as_str(),
            "backup roundtrip: timestamp not recoverable\n\
             ts:  {}\n\
             bak: {:?}",
            ts,
            bak
        );
    }

    /// Property: `check_version_compatibility` never returns `Incompatible`
    /// for any input string.
    ///
    /// From opus-review-2.md §9: the `Incompatible` variant is reserved for
    /// future use.  No current inputs should trigger it.  Any string not in
    /// the known list → `Unknown`; any in the list → `Compatible`.
    /// This property would catch accidental `Incompatible` returns.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_check_version_never_incompatible(tc: TestCase) {
        let version = tc.draw(text().max_size(40));

        // Test all adapter types that have version lists.
        for adapter in &[super::ClawKind::OpenClawHttp, super::ClawKind::NzcNative] {
            let result = super::check_version_compatibility(adapter, &version);
            assert!(
                !matches!(result, super::VersionCompatibility::Incompatible { .. }),
                "check_version_compatibility returned Incompatible for version {:?} \
                 (this variant is reserved for future use)",
                version
            );
        }
    }

    /// Property: every version in the hardcoded compatible lists is `Compatible`.
    ///
    /// From opus-review-2.md §9: for every `(adapter, version)` in the known
    /// compatible lists, the result must be `Compatible`.  This is the positive
    /// case: known versions must always be recognized.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_known_versions_always_compatible(tc: TestCase) {
        use hegel::generators as gs;

        let openclaw_versions = super::OPENCLAW_COMPATIBLE_VERSIONS;
        let nzc_versions = super::NZC_COMPATIBLE_VERSIONS;

        // Pick a random version from each list.
        let oc_idx = tc.draw(
            gs::integers::<usize>()
                .min_value(0)
                .max_value(openclaw_versions.len() - 1),
        );
        let nzc_idx = tc.draw(
            gs::integers::<usize>()
                .min_value(0)
                .max_value(nzc_versions.len() - 1),
        );

        let oc_ver = openclaw_versions[oc_idx];
        let result = super::check_version_compatibility(&super::ClawKind::OpenClawHttp, oc_ver);
        assert_eq!(
            result,
            super::VersionCompatibility::Compatible,
            "known OpenClaw version {:?} must be Compatible",
            oc_ver
        );

        let nzc_ver = nzc_versions[nzc_idx];
        let result = super::check_version_compatibility(&super::ClawKind::NzcNative, nzc_ver);
        assert_eq!(
            result,
            super::VersionCompatibility::Compatible,
            "known NZC version {:?} must be Compatible",
            nzc_ver
        );
    }
}
