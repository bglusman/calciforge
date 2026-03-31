//! Agent Alloy Provider — randomly switches between multiple LLMs per turn.
//!
//! Inspired by XBOW's "Agents Built From Alloys" research:
//! https://xbow.com/blog/alloy-agents/
//!
//! The alloy provider wraps multiple providers and randomly selects one for each
//! chat call. All models share the same conversation thread (messages history),
//! but each model thinks it wrote the previous assistant messages.

use super::traits::{ChatMessage, ChatRequest, ChatResponse, Provider};
use async_trait::async_trait;
use std::time::{SystemTime, UNIX_EPOCH};

/// An "alloy" of multiple providers — randomly selects one per turn.
///
/// Model string format: `alloy:provider1,provider2,provider3`
/// Or with specific models: `alloy:anthropic/claude-sonnet-4,google/gemini-2.5-pro`
pub struct AlloyProvider {
    /// Vec of (name, model_override, provider) tuples
    /// model_override is the specific model name to use (e.g., "claude-sonnet-4")
    providers: Vec<(String, Option<String>, Box<dyn Provider>)>,
    /// Optional weights for biased selection (default: equal)
    weights: Option<Vec<f64>>,
}

impl AlloyProvider {
    /// Create a new alloy provider from a list of (name, model_override, provider) tuples.
    pub fn new(providers: Vec<(String, Option<String>, Box<dyn Provider>)>) -> Self {
        Self {
            providers,
            weights: None,
        }
    }

    /// Create a new alloy provider with weighted selection.
    pub fn new_weighted(
        providers: Vec<(String, Option<String>, Box<dyn Provider>)>,
        weights: Vec<f64>,
    ) -> anyhow::Result<Self> {
        if providers.len() != weights.len() {
            anyhow::bail!(
                "Alloy provider count ({}) must match weight count ({})",
                providers.len(),
                weights.len()
            );
        }
        Ok(Self {
            providers,
            weights: Some(weights),
        })
    }

    /// Parse an alloy model string into provider specs.
    ///
    /// Format: `alloy:provider1/model1,provider2/model2`
    /// or: `alloy:provider1,provider2` (uses default models)
    pub fn parse_model_string(model: &str) -> anyhow::Result<Vec<(String, Option<String>)>> {
        let model = model.trim();
        
        // Strip "alloy:" prefix if present
        let spec = model.strip_prefix("alloy:").unwrap_or(model);
        
        if spec.is_empty() {
            anyhow::bail!("Alloy model string must specify at least one provider");
        }

        let providers: Vec<(String, Option<String>)> = spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                // Check for provider/model format
                if let Some((provider, model_name)) = s.split_once('/') {
                    (provider.to_string(), Some(model_name.to_string()))
                } else {
                    (s.to_string(), None)
                }
            })
            .collect();

        if providers.is_empty() {
            anyhow::bail!("Alloy model string parsed to empty provider list");
        }

        Ok(providers)
    }

    /// Select a provider index based on weights or uniform random.
    fn select_index(&self) -> usize {
        // Use a simple time-based pseudo-random pick to avoid rand version issues.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u128)
            .unwrap_or(0);
        // fold into u64
        let mut seed = (now ^ (now >> 64)) as u64;
        // mix
        seed = seed.wrapping_mul(0x9E3779B97F4A7C15).rotate_left(13);

        if let Some(ref weights) = self.weights {
            let total: f64 = weights.iter().sum();
            let frac = (seed as f64) / (u64::MAX as f64);
            let mut choice = frac * total;
            for (idx, weight) in weights.iter().enumerate() {
                choice -= *weight;
                if choice <= 0.0 {
                    return idx;
                }
            }
            self.providers.len() - 1
        } else {
            (seed as usize) % self.providers.len()
        }
    }

    /// Get the name and model of the currently selected provider (for logging).
    pub fn selected_info(&self) -> (&str, Option<&str>) {
        let idx = self.select_index();
        let (name, model, _) = &self.providers[idx];
        (name, model.as_deref())
    }
}

#[async_trait]
impl Provider for AlloyProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        // Use message hash for selection
        let idx = message.len() % self.providers.len();
        let (name, model_override, provider) = &self.providers[idx];
        let actual_model = model_override.as_deref().unwrap_or(_model);
        
        tracing::info!(provider = name.as_str(), model = actual_model, "Alloy selection");
        
        provider
            .chat_with_system(system_prompt, message, actual_model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        // Use message count for selection to vary per turn
        let idx = messages.len() % self.providers.len();
        let (name, model_override, provider) = &self.providers[idx];
        let actual_model = model_override.as_deref().unwrap_or(_model);
        
        tracing::info!(provider = name.as_str(), model = actual_model, "Alloy selection");
        
        provider
            .chat_with_history(messages, actual_model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        // Use message count for selection
        let msg_count = request.messages.len();
        let idx = msg_count % self.providers.len();
        let (name, model_override, provider) = &self.providers[idx];
        let actual_model = model_override.as_deref().unwrap_or(_model);
        
        tracing::info!(provider = name.as_str(), model = actual_model, "Alloy selection");
        
        provider.chat(request, actual_model, temperature).await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let idx = messages.len() % self.providers.len();
        let (name, model_override, provider) = &self.providers[idx];
        let actual_model = model_override.as_deref().unwrap_or(_model);
        
        tracing::info!(provider = name.as_str(), model = actual_model, "Alloy selection");
        
        provider
            .chat_with_tools(messages, tools, actual_model, temperature)
            .await
    }

    fn supports_native_tools(&self) -> bool {
        // All constituent providers must support native tools
        self.providers
            .iter()
            .all(|(_, _, p)| p.supports_native_tools())
    }

    fn supports_vision(&self) -> bool {
        // Any provider supporting vision is sufficient
        self.providers
            .iter()
            .any(|(_, _, p)| p.supports_vision())
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        for (name, model, provider) in &self.providers {
            let model_str = model.as_deref().unwrap_or("default");
            tracing::info!(provider = name, model = model_str, "Warming up alloy constituent");
            if let Err(e) = provider.warmup().await {
                tracing::warn!(provider = name, "Warmup failed (non-fatal): {e}");
            }
        }
        Ok(())
    }
}

/// Factory function to create an alloy provider from a model string.
///
/// Model format: `alloy:provider1,provider2` or `alloy:anthropic/claude-sonnet,google/gemini`
pub fn create_alloy_provider(
    model: &str,
    api_key: Option<&str>,
    options: &super::ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let specs = AlloyProvider::parse_model_string(model)?;

    if specs.len() < 2 {
        anyhow::bail!(
            "Alloy provider requires at least 2 constituent providers, got {}",
            specs.len()
        );
    }

    let mut providers: Vec<(String, Option<String>, Box<dyn Provider>)> = Vec::new();

    for (provider_name, model_override) in specs {
        // Create the constituent provider
        let provider = super::create_provider_with_options(
            &provider_name,
            api_key,
            options,
        )?;

        providers.push((provider_name, model_override, provider));
    }

    Ok(Box::new(AlloyProvider::new(providers)))
}

/// Resolve an alloy alias from the config map.
/// Returns the resolved alloy spec (with "alloy:" prefix) or the original input.
pub fn resolve_alloy_alias<'a>(
    provider: &'a str,
    aliases: &'a std::collections::HashMap<String, String>,
) -> &'a str {
    // Only resolve if it doesn't already start with "alloy:"
    if provider.starts_with("alloy:") || !aliases.contains_key(provider) {
        return provider;
    }
    // Return the aliased value
    aliases.get(provider).map(|s| s.as_str()).unwrap_or(provider)
}

/// Validate an alloy configuration at startup.
/// This creates all constituent providers to ensure they work.
pub fn validate_alloy_config(
    provider: &str,
    api_key: Option<&str>,
    options: &super::ProviderRuntimeOptions,
    aliases: &std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    let resolved = resolve_alloy_alias(provider, aliases);

    // If not an alloy after alias resolution, nothing to validate here
    if !resolved.starts_with("alloy:") {
        return Ok(());
    }

    tracing::info!(provider = provider, resolved = resolved, "Validating alloy configuration");

    let specs = AlloyProvider::parse_model_string(resolved)?;

    if specs.len() < 2 {
        anyhow::bail!(
            "Alloy provider '{}' requires at least 2 constituent providers, got {}",
            provider,
            specs.len()
        );
    }

    // Try to create each constituent provider to validate
    let mut errors = Vec::new();
    for (provider_name, model_override) in &specs {
        match super::create_provider_with_options(provider_name, api_key, options) {
            Ok(_) => {
                tracing::info!(
                    provider = provider_name,
                    model = model_override.as_deref().unwrap_or("default"),
                    "Alloy constituent validated"
                );
            }
            Err(e) => {
                tracing::error!(
                    provider = provider_name,
                    error = %e,
                    "Alloy constituent validation failed"
                );
                errors.push(format!("{}: {}", provider_name, e));
            }
        }
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "Alloy '{}' validation failed for constituents: {}",
            provider,
            errors.join(", ")
        );
    }

    tracing::info!(provider = provider, constituents = specs.len(), "Alloy configuration validated successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_alloy() {
        let result = AlloyProvider::parse_model_string("alloy:anthropic,google").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("anthropic".to_string(), None));
        assert_eq!(result[1], ("google".to_string(), None));
    }

    #[test]
    fn parse_alloy_with_models() {
        let result = AlloyProvider::parse_model_string("alloy:anthropic/claude-sonnet-4,google/gemini-2.5-pro").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("anthropic".to_string(), Some("claude-sonnet-4".to_string())));
        assert_eq!(result[1], ("google".to_string(), Some("gemini-2.5-pro".to_string())));
    }

    #[test]
    fn parse_long_model_names() {
        // Real-world long model names with version dates
        let result = AlloyProvider::parse_model_string(
            "alloy:anthropic/claude-3-5-sonnet-20241022,openai/gpt-4-turbo-2024-04-09,google/gemini-1.5-pro-002"
        ).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], ("anthropic".to_string(), Some("claude-3-5-sonnet-20241022".to_string())));
        assert_eq!(result[1], ("openai".to_string(), Some("gpt-4-turbo-2024-04-09".to_string())));
        assert_eq!(result[2], ("google".to_string(), Some("gemini-1.5-pro-002".to_string())));
    }

    #[test]
    fn parse_model_with_multiple_slashes() {
        // Model names shouldn't have multiple slashes, but verify we only split on first
        let result = AlloyProvider::parse_model_string("alloy:anthropic/claude/model-name,google/gemini").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("anthropic".to_string(), Some("claude/model-name".to_string())));
        assert_eq!(result[1], ("google".to_string(), Some("gemini".to_string())));
    }

    #[test]
    fn parse_mixed_with_and_without_models() {
        let result = AlloyProvider::parse_model_string("alloy:anthropic/claude-sonnet,google,openai/gpt-4").unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], ("anthropic".to_string(), Some("claude-sonnet".to_string())));
        assert_eq!(result[1], ("google".to_string(), None));
        assert_eq!(result[2], ("openai".to_string(), Some("gpt-4".to_string())));
    }

    #[test]
    fn parse_without_prefix() {
        let result = AlloyProvider::parse_model_string("anthropic,google,openai").unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn parse_empty_fails() {
        assert!(AlloyProvider::parse_model_string("alloy:").is_err());
        assert!(AlloyProvider::parse_model_string("").is_err());
    }

    #[test]
    fn parse_whitespace_trimmed() {
        let result = AlloyProvider::parse_model_string("alloy: anthropic , google , openai ").unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "anthropic");
        assert_eq!(result[1].0, "google");
        assert_eq!(result[2].0, "openai");
    }

    #[test]
    fn test_weighted_selection() {
        let providers = vec![
            ("a".to_string(), None, Box::new(MockProvider) as Box<dyn Provider>),
            ("b".to_string(), Some("model-b".to_string()), Box::new(MockProvider) as Box<dyn Provider>),
        ];
        let weights = vec![0.7, 0.3];
        let alloy = AlloyProvider::new_weighted(providers, weights).unwrap();
        assert!(alloy.weights.is_some());
        assert_eq!(alloy.weights.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_weighted_mismatch_fails() {
        let providers = vec![
            ("a".to_string(), None, Box::new(MockProvider) as Box<dyn Provider>),
            ("b".to_string(), None, Box::new(MockProvider) as Box<dyn Provider>),
        ];
        let weights = vec![0.5]; // wrong count
        assert!(AlloyProvider::new_weighted(providers, weights).is_err());
    }

    // Mock provider for tests
    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("mock".to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("mock".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("mock".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_native_tools(&self) -> bool {
            false
        }

        fn supports_vision(&self) -> bool {
            false
        }
    }
}
