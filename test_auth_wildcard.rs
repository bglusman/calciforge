// Test for auth wildcard handling issue
// Issue: "*" doesn't work as wildcard in model_matches function

#[cfg(test)]
mod tests {
    use zeroclawed::proxy::auth::model_matches;
    
    #[test]
    fn test_model_matches_wildcard_star() {
        // This test will FAIL with current implementation
        // "*" should match any model, but only handles "prefix/*" patterns
        assert!(model_matches("deepseek-chat", "*"), 
            "Expected '*' to match 'deepseek-chat'");
        assert!(model_matches("kimi/kimi-for-coding", "*"),
            "Expected '*' to match 'kimi/kimi-for-coding'");
        assert!(model_matches("test-alloy", "*"),
            "Expected '*' to match 'test-alloy'");
    }
    
    #[test]
    fn test_model_matches_empty_allowed_models() {
        // Test that empty allowed_models + allow_all policy works
        // This is the current workaround
    }
}