use dashmap::DashMap;
use tracing::info;

/// Credential provider — injects API keys and secrets into outgoing requests.
pub struct CredentialInjector {
    /// Map of provider name → API key (loaded from env or vault)
    credentials: DashMap<String, String>,
}

impl Default for CredentialInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialInjector {
    pub fn new() -> Self {
        Self {
            credentials: DashMap::new(),
        }
    }

    /// Load credentials from environment variables.
    ///
    /// Legacy convention: `ZEROGATE_KEY_<PROVIDER>` — populated into the
    /// cache at startup. Kept for back-compat; new code should rely on
    /// the on-demand resolver (`ensure_cached`) which looks up
    /// `<NAME>_API_KEY` (the convention used by most SDKs and by
    /// `onecli-client::vault::get_secret`). See
    /// docs/rfcs/consolidation-findings.md finding #1.
    ///
    /// Deprecation path: this method emits a per-key warning when a
    /// `ZEROGATE_KEY_*` var is found, so operators notice and can
    /// migrate to the standard form. A future PR removes this method
    /// outright once all deployments have migrated.
    pub fn load_from_env(&mut self) {
        for (key, value) in std::env::vars() {
            if let Some(provider) = key.strip_prefix("ZEROGATE_KEY_") {
                let provider_lower = provider.to_lowercase();
                tracing::warn!(
                    env_var = %key,
                    provider = %provider_lower,
                    "ZEROGATE_KEY_* is deprecated — set {}_API_KEY instead \
                     so the shared resolver (env/fnox/vault) finds it",
                    provider
                );
                self.credentials.insert(provider_lower, value);
            }
        }
    }

    /// Inject credentials into request headers based on target host.
    pub fn inject(&self, headers: &mut Vec<(String, String)>, target_host: &str) {
        let provider = self.detect_provider(target_host);
        if let Some(provider_name) = provider {
            if let Some(api_key) = self.credentials.get(&provider_name) {
                let (header_name, header_value) = self.format_auth_header(&provider_name, &api_key);
                headers.push((header_name, header_value));
                info!("Injected {} auth header for {}", provider_name, target_host);
            }
        }
    }

    /// Public wrapper so callers outside this module can use the same
    /// host→provider mapping without duplicating the pattern list.
    pub fn detect_provider_pub(&self, host: &str) -> Option<String> {
        self.detect_provider(host)
    }

    /// Detect which provider a host belongs to.
    ///
    /// Matching is suffix-bounded on a DNS label boundary, not a plain
    /// substring: `host == domain || host.ends_with(".domain")`. This
    /// prevents an attacker-registered domain like
    /// `api.openai.com.evil.example` from being identified as
    /// `openai` (which would trigger credential injection to the
    /// wrong party). A prior substring implementation had this bug;
    /// see the `detect_provider_rejects_lookalike_suffix_hosts` test.
    fn detect_provider(&self, host: &str) -> Option<String> {
        let host_lower = host.to_lowercase();
        // Table of (domain, provider-name). Order is first-match-wins;
        // put more-specific before more-general (api.github.com would
        // match before github.com if both were listed separately — but
        // we only list one per provider so it doesn't matter today).
        const PROVIDERS: &[(&str, &str)] = &[
            ("openai.com", "openai"),
            ("anthropic.com", "anthropic"),
            ("generativelanguage.googleapis.com", "google"),
            ("openrouter.ai", "openrouter"),
            ("moonshot.cn", "kimi"),
            ("github.com", "github"),
            ("cloudflare.com", "cloudflare"),
        ];
        for (domain, provider) in PROVIDERS {
            if host_lower == *domain || host_lower.ends_with(&format!(".{domain}")) {
                return Some((*provider).into());
            }
        }
        None
    }

    /// Format the auth header based on provider conventions.
    fn format_auth_header(&self, provider: &str, api_key: &str) -> (String, String) {
        match provider {
            "openai" | "openrouter" | "kimi" | "github" => {
                ("Authorization".into(), format!("Bearer {}", api_key))
            }
            "anthropic" => ("x-api-key".into(), api_key.to_string()),
            "google" | "cloudflare" => ("Authorization".into(), format!("Bearer {}", api_key)),
            _ => ("Authorization".into(), format!("Bearer {}", api_key)),
        }
    }

    /// Get a credential by provider name (for direct use).
    pub fn get(&self, provider: &str) -> Option<String> {
        self.credentials.get(provider).map(|v| v.clone())
    }

    /// Add a credential manually.
    pub fn add(&self, provider: &str, api_key: &str) {
        self.credentials
            .insert(provider.to_lowercase(), api_key.to_string());
    }

    /// Populate the cache for `provider` from the shared
    /// `onecli_client::vault::get_secret` resolver if not already
    /// present. Returns `true` when the cache has a value for the
    /// provider after the call (either it was already there or the
    /// resolver just supplied one).
    ///
    /// **Cache policy — important limitations:**
    ///
    /// - First resolve wins. Once a provider's value is cached,
    ///   `ensure_cached` returns early on every subsequent call and
    ///   does NOT re-resolve. A rotation in env/fnox/vault will not be
    ///   picked up by any call path through `ensure_cached` until the
    ///   cache is invalidated (no mechanism today) or the process
    ///   restarts.
    /// - `add(provider, value)` overwrites unconditionally. Callers
    ///   who want to rotate a credential at runtime must call `add`
    ///   directly; the resolver path alone won't refresh.
    /// - Concurrent callers on the same provider can both pass the
    ///   `contains_key` check, both call the resolver, and both insert.
    ///   Because the resolver is deterministic for a given environment,
    ///   last-write-wins on the DashMap is functionally equivalent —
    ///   wasted round-trips, but no correctness issue.
    ///
    /// **Rotation story (unchanged by this method):** runtime
    /// rotation requires adding a TTL or explicit invalidation path.
    /// Neither exists yet; rotations take effect on the next restart.
    /// This addresses finding #5 in
    /// `docs/rfcs/consolidation-findings.md` partially — the resolver
    /// is at least consulted for uncached providers per-request, which
    /// is better than the previous startup-only env scan; true
    /// rotation is follow-up work.
    pub async fn ensure_cached(&self, provider: &str) -> bool {
        let key = provider.to_lowercase();
        if self.credentials.contains_key(&key) {
            return true;
        }
        match onecli_client::vault::get_secret(&key).await {
            Ok(secret) => {
                self.credentials.insert(key, secret);
                true
            }
            Err(e) => {
                tracing::debug!(
                    provider = %provider,
                    error = %e,
                    "ensure_cached: resolver returned no secret"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_provider() {
        let injector = CredentialInjector::new();
        assert_eq!(
            injector.detect_provider("api.openai.com"),
            Some("openai".into())
        );
        assert_eq!(
            injector.detect_provider("api.anthropic.com"),
            Some("anthropic".into())
        );
        assert_eq!(
            injector.detect_provider("generativelanguage.googleapis.com"),
            Some("google".into())
        );
        assert_eq!(
            injector.detect_provider("openrouter.ai"),
            Some("openrouter".into())
        );
        assert_eq!(injector.detect_provider("example.com"), None);
    }

    #[test]
    fn test_format_auth_header() {
        let injector = CredentialInjector::new();

        let (name, value) = injector.format_auth_header("openai", "sk-test123");
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer sk-test123");

        let (name, value) = injector.format_auth_header("anthropic", "sk-ant-test");
        assert_eq!(name, "x-api-key");
        assert_eq!(value, "sk-ant-test");
    }

    #[test]
    fn test_inject_no_credential() {
        let injector = CredentialInjector::new();
        let mut headers = vec![];
        injector.inject(&mut headers, "api.openai.com");
        assert!(headers.is_empty());
    }

    #[test]
    fn test_inject_with_credential() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-test123");

        let mut headers = vec![];
        injector.inject(&mut headers, "api.openai.com");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-test123");
    }

    #[test]
    fn test_get_credential() {
        let injector = CredentialInjector::new();
        injector.add("github", "ghp_test");

        assert_eq!(injector.get("github"), Some("ghp_test".into()));
        assert_eq!(injector.get("missing"), None);
    }

    #[test]
    fn test_add_overwrites() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-old");
        injector.add("openai", "sk-new");

        assert_eq!(injector.get("openai"), Some("sk-new".into()));
    }

    /// Given a cache that already contains a credential for a provider,
    /// when ensure_cached is called,
    /// then it returns true without touching the resolver.
    ///
    /// This confirms first-write-wins and protects against a subtle
    /// regression where ensure_cached re-resolves unconditionally —
    /// which would (a) mask rotation via direct `add()`, (b) pay a
    /// resolver-round-trip on every request.
    #[tokio::test]
    async fn ensure_cached_skips_resolver_when_already_cached() {
        let injector = CredentialInjector::new();
        injector.add("openai", "sk-from-add");

        let resolved = injector.ensure_cached("openai").await;

        assert!(resolved, "should report success when value is cached");
        assert_eq!(
            injector.get("openai"),
            Some("sk-from-add".into()),
            "cached value must not be overwritten by a successful resolver call"
        );
    }

    /// Given a URL host that contains a provider name as a substring
    /// but is NOT a legitimate subdomain of that provider,
    /// when detect_provider is called,
    /// then it returns None.
    ///
    /// This is a security-critical assertion. A naive substring match
    /// against `host.contains("openai.com")` would happily match
    /// `api.openai.com.evil.example` and inject the user's OpenAI
    /// credential into a request to the attacker's server. The match
    /// must be suffix-bounded on a dot boundary (host is exactly the
    /// domain, or ends with `.<domain>`). Discovered by the
    /// test-quality audit subagent on 2026-04-24.
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
            assert_eq!(
                injector.detect_provider(host),
                None,
                "lookalike host {host:?} must NOT be identified as a known provider — \
                 a non-None here means an attacker who registers a .evil.example \
                 subdomain can trigger credential injection for the named provider"
            );
        }
    }

    /// Positive companion to the above: legitimate subdomains must still
    /// be detected. `api.openai.com` is openai; the bare `openai.com`
    /// is openai too (some legitimate callers may hit the apex).
    #[test]
    fn detect_provider_accepts_real_subdomains() {
        let injector = CredentialInjector::new();
        let cases = [
            ("api.openai.com", "openai"),
            ("openai.com", "openai"),
            ("api.anthropic.com", "anthropic"),
            ("api.github.com", "github"),
            ("github.com", "github"),
        ];
        for (host, expected) in cases {
            assert_eq!(
                injector.detect_provider(host),
                Some(expected.into()),
                "legitimate host {host:?} must be detected as {expected:?}"
            );
        }
    }

    /// Given a cache that has no entry for the provider,
    /// and the resolver has no secret either (nothing in env/fnox/vault),
    /// when ensure_cached is called,
    /// then it returns false and the cache stays empty.
    ///
    /// Catches a regression where a resolver failure leaks an empty or
    /// error-stringified value into the cache and a subsequent inject()
    /// sends `Authorization: Bearer ` (empty) to the upstream.
    ///
    /// Hermetic setup: we derive a per-run provider name from the
    /// process ID so this test doesn't collide with anything real in
    /// the dev/CI environment, and we explicitly clear the vault env
    /// so `get_secret` short-circuits to "not found" rather than
    /// attempting a real network call.
    #[tokio::test]
    async fn ensure_cached_returns_false_when_nothing_resolves() {
        // Safety: env mutation in tests is inherently process-global;
        // using unsafe to satisfy Rust 2024's edition semantics. The
        // specific variables we touch aren't read by concurrent tests
        // in this module.
        unsafe {
            std::env::remove_var("ONECLI_VAULT_TOKEN");
            std::env::remove_var("ONECLI_VAULT_URL");
        }
        let provider_name = format!("nosuchprovider_pid_{}", std::process::id());
        let injector = CredentialInjector::new();
        let resolved = injector.ensure_cached(&provider_name).await;

        assert!(!resolved, "should return false when nothing resolved");
        assert_eq!(
            injector.get(&provider_name),
            None,
            "failed lookup must not leave a stub entry in the cache"
        );
    }
}
