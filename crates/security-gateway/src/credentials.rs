#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_provider() {
        let injector = CredentialInjector::new();
        assert_eq!(injector.detect_provider("api.openai.com"), Some("openai".into()));
        assert_eq!(injector.detect_provider("api.anthropic.com"), Some("anthropic".into()));
        assert_eq!(injector.detect_provider("generativelanguage.googleapis.com"), Some("google".into()));
        assert_eq!(injector.detect_provider("openrouter.ai"), Some("openrouter".into()));
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
        assert!(headers.is_empty()); // No credential loaded
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
