//! Named security profiles for ZeroClawed installation.
//!
//! Each profile bundles a set of reasonable defaults for different paranoia levels.
//! Profiles are composable — users can pick a base profile and override individual fields.
//!
//! # Profiles
//!
//! | Profile | Digest | Discussion | Override | Tools | Rate Limit | Logging |
//! |---------|--------|------------|----------|-------|------------|---------|
//! | `Open` | ✅ | 0.5 | true | web only | generous | minimal |
//! | `Balanced` | ✅ | 0.3 | false | fetch+search | moderate | standard |
//! | `Hardened` | ✅ | 0.15 | false | all tools | strict | verbose |
//! | `Paranoid` | ❌ | 0.0 | false | all + exec | aggressive | trace |

use serde::{Deserialize, Serialize};

use crate::middleware::InterceptedToolSet;
use crate::scanner::ScannerConfig;

/// The named security profile level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityProfile {
    /// Minimal scanning, permissive. Best for development or trusted environments.
    ///
    /// - Only scans `web_fetch` content (not search results or exec output)
    /// - Discussion context aggressively downgrades unsafe → review
    /// - Review verdicts auto-pass (no human approval needed)
    /// - Generous rate limits (120 req/min)
    /// - Minimal logging
    Open,
    /// Reasonable defaults for most production deployments.
    ///
    /// - Scans `web_fetch`, `web_search`, `safe_fetch`
    /// - Standard discussion context heuristic (0.3 ratio)
    /// - Review verdicts require explicit approval
    /// - Moderate rate limits (60 req/min)
    /// - Standard logging
    Balanced,
    /// Stricter scanning for security-conscious environments.
    ///
    /// - Scans all tool results including `email_fetch` and `exec`
    /// - Tighter discussion context (0.15 ratio — less likely to downgrade)
    /// - Review verdicts blocked by default
    /// - Strict rate limits (30 req/min)
    /// - Verbose logging with content snippets
    Hardened,
    /// Maximum protection. High false-positive rate expected.
    ///
    /// - All tools intercepted, including `exec` output scanning
    /// - No discussion context heuristic (0.0 — never downgrade)
    /// - Review treated as unsafe (blocked)
    /// - Aggressive rate limiting (15 req/min)
    /// - Full trace logging
    /// - Digest cache disabled (every fetch is rescanned)
    Paranoid,
}

impl Default for SecurityProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

impl std::str::FromStr for SecurityProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "open" | "permissive" | "relaxed" => Ok(Self::Open),
            "balanced" | "standard" | "default" => Ok(Self::Balanced),
            "hardened" | "strict" | "secure" => Ok(Self::Hardened),
            "paranoid" | "maximum" => Ok(Self::Paranoid),
            _ => Err(format!(
                "unknown security profile '{s}'. Valid: open, balanced, hardened, paranoid"
            )),
        }
    }
}

impl std::fmt::Display for SecurityProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Balanced => write!(f, "balanced"),
            Self::Hardened => write!(f, "hardened"),
            Self::Paranoid => write!(f, "paranoid"),
        }
    }
}

/// Rate limiting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum requests per minute per source.
    pub max_requests_per_minute: u32,
    /// Burst allowance (requests that can exceed the rate briefly).
    pub burst_size: u32,
    /// Cooldown after hitting the limit (seconds).
    pub cooldown_seconds: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self::balanced()
    }
}

impl RateLimitConfig {
    pub fn open() -> Self {
        Self {
            max_requests_per_minute: 120,
            burst_size: 20,
            cooldown_seconds: 5,
        }
    }

    pub fn balanced() -> Self {
        Self {
            max_requests_per_minute: 60,
            burst_size: 10,
            cooldown_seconds: 10,
        }
    }

    pub fn hardened() -> Self {
        Self {
            max_requests_per_minute: 30,
            burst_size: 5,
            cooldown_seconds: 15,
        }
    }

    pub fn paranoid() -> Self {
        Self {
            max_requests_per_minute: 15,
            burst_size: 2,
            cooldown_seconds: 30,
        }
    }
}

/// Logging verbosity for the security pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogVerbosity {
    /// Only log verdicts (clean/review/unsafe) without content details.
    Minimal,
    /// Log verdicts with URL and brief reason.
    Standard,
    /// Log verdicts with URL, reason, content snippet, and layer that triggered.
    Verbose,
    /// Full trace: every scan, every pattern match, every cache hit/miss.
    Trace,
}

impl Default for LogVerbosity {
    fn default() -> Self {
        Self::Standard
    }
}

/// Complete security configuration derived from a named profile.
///
/// This is the single source of truth for all security-related settings.
/// Construct from a [`SecurityProfile`] or override individual fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecurityConfig {
    /// The named profile this config was derived from (for display/logging).
    pub profile: SecurityProfile,

    /// Scanner configuration (layer thresholds, service URL, etc.)
    pub scanner: ScannerConfig,

    /// Which tool results to intercept and scan.
    pub intercepted_tools: InterceptedToolSet,

    /// Rate limiting per source.
    pub rate_limit: RateLimitConfig,

    /// Logging verbosity.
    pub log_verbosity: LogVerbosity,

    /// Whether to enable the digest cache. Disabling forces a rescan every time.
    /// Default: `true` for all profiles except `Paranoid`.
    pub enable_digest_cache: bool,

    /// Maximum age of a digest cache entry before it's rescanned (seconds).
    /// `0` = never expires (only content-hash invalidates).
    pub digest_cache_ttl_secs: u64,

    /// Whether to scan outbound messages (agent → user) in addition to inbound.
    /// Default: `false` for `Open`/`Balanced`, `true` for `Hardened`/`Paranoid`.
    pub scan_outbound: bool,

    /// Whether to enable audit logging of all security decisions.
    pub audit_logging: bool,
}

impl SecurityConfig {
    /// Build a config from a named profile.
    pub fn from_profile(profile: SecurityProfile) -> Self {
        match profile {
            SecurityProfile::Open => Self::open(),
            SecurityProfile::Balanced => Self::balanced(),
            SecurityProfile::Hardened => Self::hardened(),
            SecurityProfile::Paranoid => Self::paranoid(),
        }
    }

    pub fn open() -> Self {
        Self {
            profile: SecurityProfile::Open,
            scanner: ScannerConfig {
                discussion_ratio_threshold: 0.5,
                min_signals_for_ratio: 2,
                override_on_review: true,
                ..Default::default()
            },
            intercepted_tools: InterceptedToolSet::web_only(),
            rate_limit: RateLimitConfig::open(),
            log_verbosity: LogVerbosity::Minimal,
            enable_digest_cache: true,
            digest_cache_ttl_secs: 86400, // 24h
            scan_outbound: false,
            audit_logging: false,
        }
    }

    pub fn balanced() -> Self {
        Self {
            profile: SecurityProfile::Balanced,
            scanner: ScannerConfig {
                discussion_ratio_threshold: 0.3,
                min_signals_for_ratio: 3,
                override_on_review: false,
                ..Default::default()
            },
            intercepted_tools: InterceptedToolSet::web_and_search(),
            rate_limit: RateLimitConfig::balanced(),
            log_verbosity: LogVerbosity::Standard,
            enable_digest_cache: true,
            digest_cache_ttl_secs: 3600, // 1h
            scan_outbound: false,
            audit_logging: true,
        }
    }

    pub fn hardened() -> Self {
        Self {
            profile: SecurityProfile::Hardened,
            scanner: ScannerConfig {
                discussion_ratio_threshold: 0.15,
                min_signals_for_ratio: 5,
                override_on_review: false,
                ..Default::default()
            },
            intercepted_tools: InterceptedToolSet::all_tools(),
            rate_limit: RateLimitConfig::hardened(),
            log_verbosity: LogVerbosity::Verbose,
            enable_digest_cache: true,
            digest_cache_ttl_secs: 300, // 5min
            scan_outbound: true,
            audit_logging: true,
        }
    }

    pub fn paranoid() -> Self {
        Self {
            profile: SecurityProfile::Paranoid,
            scanner: ScannerConfig {
                discussion_ratio_threshold: 0.0,
                min_signals_for_ratio: 0,
                override_on_review: false,
                ..Default::default()
            },
            intercepted_tools: InterceptedToolSet::all_including_exec(),
            rate_limit: RateLimitConfig::paranoid(),
            log_verbosity: LogVerbosity::Trace,
            enable_digest_cache: false,
            digest_cache_ttl_secs: 0,
            scan_outbound: true,
            audit_logging: true,
        }
    }

    /// Short human description of this profile.
    pub fn description(&self) -> &'static str {
        match self.profile {
            SecurityProfile::Open => {
                "Minimal scanning. Best for development or trusted environments. \
                 Review verdicts auto-pass. Generous rate limits."
            }
            SecurityProfile::Balanced => {
                "Reasonable defaults for production. Scans web fetches and search results. \
                 Review verdicts require approval. Moderate rate limits."
            }
            SecurityProfile::Hardened => {
                "Stricter scanning for security-conscious deployments. All tools intercepted. \
                 Tighter heuristics. Verbose logging."
            }
            SecurityProfile::Paranoid => {
                "Maximum protection. Every fetch rescanned. No context heuristics. \
                 High false-positive rate expected. Full trace logging."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_from_str() {
        assert_eq!("open".parse::<SecurityProfile>().unwrap(), SecurityProfile::Open);
        assert_eq!("balanced".parse::<SecurityProfile>().unwrap(), SecurityProfile::Balanced);
        assert_eq!("hardened".parse::<SecurityProfile>().unwrap(), SecurityProfile::Hardened);
        assert_eq!("paranoid".parse::<SecurityProfile>().unwrap(), SecurityProfile::Paranoid);
        assert_eq!("strict".parse::<SecurityProfile>().unwrap(), SecurityProfile::Hardened);
        assert_eq!("relaxed".parse::<SecurityProfile>().unwrap(), SecurityProfile::Open);
    }

    #[test]
    fn test_profile_from_str_invalid() {
        let result = "yolo".parse::<SecurityProfile>();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("open, balanced, hardened, paranoid"));
    }

    #[test]
    fn test_all_profiles_build() {
        for p in [SecurityProfile::Open, SecurityProfile::Balanced, SecurityProfile::Hardened, SecurityProfile::Paranoid] {
            let config = SecurityConfig::from_profile(p);
            assert_eq!(config.profile, p);
            assert!(!config.description().is_empty());
        }
    }

    #[test]
    fn test_open_is_permissive() {
        let config = SecurityConfig::open();
        assert!(config.scanner.override_on_review);
        assert_eq!(config.scanner.discussion_ratio_threshold, 0.5);
        assert!(!config.scan_outbound);
        assert!(!config.audit_logging);
    }

    #[test]
    fn test_paranoid_is_strict() {
        let config = SecurityConfig::paranoid();
        assert!(!config.scanner.override_on_review);
        assert_eq!(config.scanner.discussion_ratio_threshold, 0.0);
        assert!(!config.enable_digest_cache);
        assert!(config.scan_outbound);
        assert_eq!(config.rate_limit.max_requests_per_minute, 15);
    }

    #[test]
    fn test_profiles_are_progressively_stricter() {
        let open = SecurityConfig::open();
        let balanced = SecurityConfig::balanced();
        let hardened = SecurityConfig::hardened();
        let paranoid = SecurityConfig::paranoid();

        // Discussion ratio: higher = more permissive
        assert!(open.scanner.discussion_ratio_threshold > balanced.scanner.discussion_ratio_threshold);
        assert!(balanced.scanner.discussion_ratio_threshold > hardened.scanner.discussion_ratio_threshold);
        assert!(hardened.scanner.discussion_ratio_threshold > paranoid.scanner.discussion_ratio_threshold);

        // Rate limits: higher = more permissive
        assert!(open.rate_limit.max_requests_per_minute > balanced.rate_limit.max_requests_per_minute);
        assert!(balanced.rate_limit.max_requests_per_minute > hardened.rate_limit.max_requests_per_minute);
        assert!(hardened.rate_limit.max_requests_per_minute > paranoid.rate_limit.max_requests_per_minute);
    }
}
