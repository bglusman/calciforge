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
    /// Pattern: ZEROGATE_KEY_<PROVIDER> = <api_key>
    pub fn load_from_env(&mut self) {
        for (key, value) in std::env::vars() {
            if let Some(provider) = key.strip_prefix("ZEROGATE_KEY_") {
                let provider = provider.to_lowercase();
                info!("Loaded credential for provider: {}", provider);
                self.credentials.insert(provider, value);
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

    /// Detect which provider a host belongs to.
    fn detect_provider(&self, host: &str) -> Option<String> {
        let host_lower = host.to_lowercase();
        if host_lower.contains("openai.com") {
            Some("openai".into())
        } else if host_lower.contains("api.anthropic.com") {
            Some("anthropic".into())
        } else if host_lower.contains("generativelanguage.googleapis.com") {
            Some("google".into())
        } else if host_lower.contains("openrouter.ai") {
            Some("openrouter".into())
        } else if host_lower.contains("api.moonshot.cn") || host_lower.contains("kimi.moonshot.cn")
        {
            Some("kimi".into())
        } else if host_lower.contains("api.github.com") || host_lower.contains("github.com") {
            Some("github".into())
        } else if host_lower.contains("api.cloudflare.com") || host_lower.contains("cloudflare.com")
        {
            Some("cloudflare".into())
        } else {
            None
        }
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
}
