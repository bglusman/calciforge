use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// How a credential is injected into the outgoing request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InjectionMethod {
    /// Authorization: Bearer <secret>
    Bearer,
    /// Authorization: Basic base64(<username>:<secret>)
    Basic { username: String },
    /// Custom header: <name>: <prefix><secret>
    Header {
        name: String,
        #[serde(default)]
        prefix: String,
    },
    /// Query parameter: ?<name>=<secret>
    QueryParam { name: String },
}

/// A host→credential mapping entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialMapping {
    /// DNS-boundary-safe host patterns.
    /// - "openai.com" matches openai.com and *.openai.com
    /// - "*.corp.example.com" matches any subdomain of corp.example.com
    pub hosts: Vec<String>,
    /// Secret name passed to `secrets_client::vault::get_secret`
    pub secret_name: String,
    /// How to inject the resolved secret
    pub injection: InjectionMethod,
}

/// Top-level credentials config (loaded from TOML).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialsConfig {
    #[serde(default)]
    pub mappings: Vec<CredentialMapping>,
    /// Cache TTL in seconds. 0 = no expiry. Default: 300 (5 min).
    #[serde(default = "default_ttl")]
    pub cache_ttl_secs: u64,
}

fn default_ttl() -> u64 {
    300
}

struct CachedSecret {
    value: String,
    resolved_at: Instant,
}

/// Credential provider — injects API keys and secrets into outgoing requests
/// based on configurable host→credential mappings.
pub struct CredentialInjector {
    cache: DashMap<String, CachedSecret>,
    mappings: Vec<CredentialMapping>,
    cache_ttl: Duration,
}

impl Default for CredentialInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialInjector {
    /// Create with built-in provider mappings (backward-compatible default).
    pub fn new() -> Self {
        Self::with_config(None)
    }

    /// Create with explicit configuration. If `None`, uses the built-in table.
    pub fn with_config(config: Option<CredentialsConfig>) -> Self {
        let (mappings, ttl) = match config {
            Some(cfg) => {
                let ttl = Duration::from_secs(cfg.cache_ttl_secs);
                (cfg.mappings, ttl)
            }
            None => (Self::builtin_mappings(), Duration::ZERO),
        };
        info!(
            mappings = mappings.len(),
            ttl_secs = ttl.as_secs(),
            "credential injector initialized"
        );
        Self {
            cache: DashMap::new(),
            mappings,
            cache_ttl: ttl,
        }
    }

    /// Load from a TOML file. Returns `None` if file doesn't exist.
    pub fn load_config(path: &str) -> Option<CredentialsConfig> {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(cfg) => {
                    info!(path = %path, "loaded credentials config");
                    Some(cfg)
                }
                Err(e) => {
                    warn!(path = %path, error = %e, "failed to parse credentials config");
                    None
                }
            },
            Err(_) => None,
        }
    }

    /// Load credentials from legacy `ZEROGATE_KEY_*` environment variables.
    pub fn load_from_env(&mut self) {
        for (key, value) in std::env::vars() {
            if let Some(provider) = key.strip_prefix("ZEROGATE_KEY_") {
                let provider_lower = provider.to_lowercase();
                warn!(
                    env_var = %key,
                    provider = %provider_lower,
                    "ZEROGATE_KEY_* is deprecated — set {}_API_KEY instead",
                    provider
                );
                self.add(&provider_lower, &value);
            }
        }
    }

    /// Inject credentials into request headers based on target host.
    ///
    /// Resolves secrets on-demand from the vault with TTL-based caching.
    pub async fn inject(&self, headers: &mut Vec<(String, String)>, target_host: &str) {
        let mapping = self.find_mapping(target_host);
        if let Some(m) = mapping {
            if let Some(secret) = self.resolve_secret(&m.secret_name).await {
                let (header_name, header_value) = format_injection(&m.injection, &secret);
                headers.push((header_name, header_value));
                info!(
                    secret_name = %m.secret_name,
                    host = %target_host,
                    "injected credential"
                );
            }
        }
    }

    /// Find which mapping matches this host (first match wins).
    pub fn find_mapping(&self, host: &str) -> Option<&CredentialMapping> {
        let host_lower = host.to_lowercase();
        self.mappings.iter().find(|m| {
            m.hosts
                .iter()
                .any(|pattern| dns_boundary_match(&host_lower, pattern))
        })
    }

    /// Public accessor: which secret name would be injected for this host?
    pub fn detect_provider_pub(&self, host: &str) -> Option<String> {
        self.find_mapping(host).map(|m| m.secret_name.clone())
    }

    /// Get a credential by secret name (for direct use).
    pub fn get(&self, secret_name: &str) -> Option<String> {
        let key = secret_name.to_lowercase();
        self.cache.get(&key).map(|v| v.value.clone())
    }

    /// Add a credential manually (bypasses vault resolution).
    pub fn add(&self, secret_name: &str, value: &str) {
        self.cache.insert(
            secret_name.to_lowercase(),
            CachedSecret {
                value: value.to_string(),
                resolved_at: Instant::now(),
            },
        );
    }

    /// Ensure a secret is cached (resolve from vault if missing or expired).
    pub async fn ensure_cached(&self, secret_name: &str) -> bool {
        self.resolve_secret(secret_name).await.is_some()
    }

    async fn resolve_secret(&self, secret_name: &str) -> Option<String> {
        let key = secret_name.to_lowercase();

        // Check cache (with TTL)
        if let Some(entry) = self.cache.get(&key) {
            if self.cache_ttl.is_zero() || entry.resolved_at.elapsed() < self.cache_ttl {
                return Some(entry.value.clone());
            }
            // Expired — fall through to re-resolve
        }

        // Resolve from vault
        match secrets_client::vault::get_secret(&key).await {
            Ok(secret) => {
                debug!(secret_name = %secret_name, "credential resolved from vault");
                self.cache.insert(
                    key,
                    CachedSecret {
                        value: secret.clone(),
                        resolved_at: Instant::now(),
                    },
                );
                Some(secret)
            }
            Err(e) => {
                // On refresh failure, use stale value if available
                if let Some(entry) = self.cache.get(&key) {
                    warn!(
                        secret_name = %secret_name,
                        error = %e,
                        "vault refresh failed, using stale cached value"
                    );
                    return Some(entry.value.clone());
                }
                debug!(
                    secret_name = %secret_name,
                    error = %e,
                    "no secret resolved"
                );
                None
            }
        }
    }

    fn builtin_mappings() -> Vec<CredentialMapping> {
        vec![
            CredentialMapping {
                hosts: vec!["openai.com".into()],
                secret_name: "openai".into(),
                injection: InjectionMethod::Bearer,
            },
            CredentialMapping {
                hosts: vec!["anthropic.com".into()],
                secret_name: "anthropic".into(),
                injection: InjectionMethod::Header {
                    name: "x-api-key".into(),
                    prefix: String::new(),
                },
            },
            CredentialMapping {
                hosts: vec!["generativelanguage.googleapis.com".into()],
                secret_name: "google".into(),
                injection: InjectionMethod::Bearer,
            },
            CredentialMapping {
                hosts: vec!["openrouter.ai".into()],
                secret_name: "openrouter".into(),
                injection: InjectionMethod::Bearer,
            },
            CredentialMapping {
                hosts: vec!["moonshot.cn".into()],
                secret_name: "kimi".into(),
                injection: InjectionMethod::Bearer,
            },
            CredentialMapping {
                hosts: vec!["github.com".into()],
                secret_name: "github".into(),
                injection: InjectionMethod::Bearer,
            },
            CredentialMapping {
                hosts: vec!["cloudflare.com".into()],
                secret_name: "cloudflare".into(),
                injection: InjectionMethod::Bearer,
            },
        ]
    }
}

/// DNS-boundary-safe host matching.
///
/// Pattern forms:
/// - `*.example.com` — matches any subdomain but NOT example.com itself
/// - `example.com` — matches example.com AND any subdomain (*.example.com)
/// - Exact match for fully qualified hosts
///
/// Prevents credential injection to lookalike domains (e.g.,
/// `api.openai.com.evil.example` will NOT match `openai.com`).
fn dns_boundary_match(host: &str, pattern: &str) -> bool {
    let host_lower = host.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    if let Some(suffix) = pattern_lower.strip_prefix("*.") {
        // Glob pattern: match any subdomain of suffix, but not suffix itself
        host_lower.ends_with(&format!(".{suffix}")) && host_lower.len() > suffix.len() + 1
    } else {
        // Bare domain: match exact OR any subdomain
        host_lower == pattern_lower || host_lower.ends_with(&format!(".{pattern_lower}"))
    }
}

/// Format the injection based on method and secret value.
fn format_injection(method: &InjectionMethod, secret: &str) -> (String, String) {
    match method {
        InjectionMethod::Bearer => ("Authorization".into(), format!("Bearer {secret}")),
        InjectionMethod::Basic { username } => {
            use base64::Engine;
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"));
            ("Authorization".into(), format!("Basic {encoded}"))
        }
        InjectionMethod::Header { name, prefix } => (name.clone(), format!("{prefix}{secret}")),
        InjectionMethod::QueryParam { .. } => {
            // Query params are handled separately by the caller since they
            // modify the URL, not headers. Return empty — caller checks.
            (String::new(), String::new())
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_match_bare_domain_matches_exact() {
        assert!(dns_boundary_match("openai.com", "openai.com"));
    }

    #[test]
    fn dns_match_bare_domain_matches_subdomain() {
        assert!(dns_boundary_match("api.openai.com", "openai.com"));
    }

    #[test]
    fn dns_match_glob_matches_subdomain() {
        assert!(dns_boundary_match(
            "api.corp.example.com",
            "*.corp.example.com"
        ));
    }

    #[test]
    fn dns_match_glob_rejects_bare() {
        assert!(!dns_boundary_match(
            "corp.example.com",
            "*.corp.example.com"
        ));
    }

    #[test]
    fn dns_match_rejects_lookalike_suffix() {
        assert!(!dns_boundary_match(
            "api.openai.com.evil.example",
            "openai.com"
        ));
        assert!(!dns_boundary_match(
            "openai.com.attacker.test",
            "openai.com"
        ));
        assert!(!dns_boundary_match(
            "api.anthropic.com.evil.xyz",
            "anthropic.com"
        ));
    }

    #[test]
    fn dns_match_case_insensitive() {
        assert!(dns_boundary_match("API.OpenAI.COM", "openai.com"));
    }

    #[test]
    fn builtin_mappings_cover_known_providers() {
        let injector = CredentialInjector::new();
        let cases = [
            ("api.openai.com", "openai"),
            ("openai.com", "openai"),
            ("api.anthropic.com", "anthropic"),
            ("generativelanguage.googleapis.com", "google"),
            ("openrouter.ai", "openrouter"),
            ("api.moonshot.cn", "kimi"),
            ("github.com", "github"),
            ("api.github.com", "github"),
            ("cloudflare.com", "cloudflare"),
        ];
        for (host, expected) in cases {
            let mapping = injector.find_mapping(host);
            assert!(mapping.is_some(), "host {host:?} should match a mapping");
            assert_eq!(
                mapping.unwrap().secret_name,
                expected,
                "host {host:?} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn builtin_rejects_unknown_hosts() {
        let injector = CredentialInjector::new();
        assert!(injector.find_mapping("example.com").is_none());
        assert!(injector.find_mapping("random.test").is_none());
    }

    #[test]
    fn format_bearer() {
        let (name, value) = format_injection(&InjectionMethod::Bearer, "sk-test");
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer sk-test");
    }

    #[test]
    fn format_header_with_prefix() {
        let method = InjectionMethod::Header {
            name: "x-api-key".into(),
            prefix: String::new(),
        };
        let (name, value) = format_injection(&method, "sk-ant-123");
        assert_eq!(name, "x-api-key");
        assert_eq!(value, "sk-ant-123");
    }

    #[test]
    fn format_header_with_custom_prefix() {
        let method = InjectionMethod::Header {
            name: "X-Custom-Auth".into(),
            prefix: "Token ".into(),
        };
        let (name, value) = format_injection(&method, "abc123");
        assert_eq!(name, "X-Custom-Auth");
        assert_eq!(value, "Token abc123");
    }

    #[test]
    fn add_and_get() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-test");
        assert_eq!(injector.get("openai"), Some("sk-test".into()));
        assert_eq!(injector.get("missing"), None);
    }

    #[test]
    fn add_overwrites() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-old");
        injector.add("openai", "sk-new");
        assert_eq!(injector.get("openai"), Some("sk-new".into()));
    }

    #[test]
    fn config_from_toml() {
        let toml_str = r#"
cache_ttl_secs = 60

[[mappings]]
hosts = ["api.custom.com", "*.custom.com"]
secret_name = "custom_api"
injection = { type = "bearer" }

[[mappings]]
hosts = ["internal.corp.example.com"]
secret_name = "corp_key"
injection = { type = "header", name = "X-Corp-Key", prefix = "" }
"#;
        let config: CredentialsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.cache_ttl_secs, 60);
        assert_eq!(config.mappings.len(), 2);
        assert_eq!(config.mappings[0].secret_name, "custom_api");
        assert_eq!(
            config.mappings[1].injection,
            InjectionMethod::Header {
                name: "X-Corp-Key".into(),
                prefix: String::new(),
            }
        );
    }

    #[test]
    fn custom_config_matching() {
        let config = CredentialsConfig {
            mappings: vec![
                CredentialMapping {
                    hosts: vec!["*.internal.corp".into()],
                    secret_name: "corp".into(),
                    injection: InjectionMethod::Bearer,
                },
                CredentialMapping {
                    hosts: vec!["special.api.com".into()],
                    secret_name: "special".into(),
                    injection: InjectionMethod::Header {
                        name: "X-Key".into(),
                        prefix: "Key ".into(),
                    },
                },
            ],
            cache_ttl_secs: 60,
        };
        let injector = CredentialInjector::with_config(Some(config));

        assert_eq!(
            injector
                .find_mapping("foo.internal.corp")
                .unwrap()
                .secret_name,
            "corp"
        );
        assert_eq!(
            injector
                .find_mapping("special.api.com")
                .unwrap()
                .secret_name,
            "special"
        );
        assert!(injector.find_mapping("other.com").is_none());
    }

    #[tokio::test]
    async fn ensure_cached_with_manual_add() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-manual");
        assert!(injector.ensure_cached("openai").await);
        assert_eq!(injector.get("openai"), Some("sk-manual".into()));
    }

    #[tokio::test]
    async fn ensure_cached_returns_false_when_nothing_resolves() {
        unsafe {
            std::env::remove_var("SECRETS_VAULT_TOKEN");
            std::env::remove_var("SECRETS_VAULT_URL");
        }
        let provider_name = format!("nosuchprovider_pid_{}", std::process::id());
        let injector = CredentialInjector::new();
        let resolved = injector.ensure_cached(&provider_name).await;
        assert!(!resolved);
        assert_eq!(injector.get(&provider_name), None);
    }

    #[tokio::test]
    async fn inject_with_cached_credential() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-test123");

        let mut headers = vec![];
        injector.inject(&mut headers, "api.openai.com").await;
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-test123");
    }

    #[tokio::test]
    async fn inject_anthropic_uses_xapikey() {
        let injector = CredentialInjector::new();
        injector.add("anthropic", "sk-ant-test");

        let mut headers = vec![];
        injector.inject(&mut headers, "api.anthropic.com").await;
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-api-key");
        assert_eq!(headers[0].1, "sk-ant-test");
    }

    #[tokio::test]
    async fn inject_no_credential_no_header() {
        let injector = CredentialInjector::new();
        let mut headers = vec![];
        injector.inject(&mut headers, "api.openai.com").await;
        assert!(headers.is_empty());
    }

    #[test]
    fn detect_provider_rejects_lookalike_suffix_hosts() {
        let injector = CredentialInjector::new();
        let lookalikes = [
            "api.openai.com.evil.example",
            "openai.com.attacker.test",
            "api.anthropic.com.evil.xyz",
            "openrouter.ai.evil.test",
            "github.com.phish.example",
            "generativelanguage.googleapis.com.attacker.test",
        ];
        for host in lookalikes {
            assert!(
                injector.find_mapping(host).is_none(),
                "lookalike host {host:?} must NOT match any mapping"
            );
        }
    }
}
