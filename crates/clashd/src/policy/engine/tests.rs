//! Tests for PolicyEngine

use super::*;
use crate::Verdict;
use std::path::PathBuf;
use tempfile::TempDir;

async fn create_test_policy(dir: &TempDir, content: &str) -> PathBuf {
    let path = dir.path().join("test_policy.star");
    tokio::fs::write(&path, content).await.unwrap();
    path
}

#[tokio::test]
async fn test_engine_allows_by_default() {
    let tmp = TempDir::new().unwrap();
    let policy = create_test_policy(
        &tmp,
        r#"
def evaluate(tool, args, context):
    return "allow"
"#,
    )
    .await;

    let engine = PolicyEngine::new(&policy).await.unwrap();
    let result = engine.evaluate("test", &serde_json::json!({}), None).await;

    assert_eq!(result.verdict, Verdict::Allow);
}

#[tokio::test]
async fn test_engine_denies_when_policy_returns_deny() {
    let tmp = TempDir::new().unwrap();
    let policy = create_test_policy(
        &tmp,
        r#"
def evaluate(tool, args, context):
    return {"verdict": "deny", "reason": "test denial"}
"#,
    )
    .await;

    let engine = PolicyEngine::new(&policy).await.unwrap();
    let result = engine.evaluate("test", &serde_json::json!({}), None).await;

    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(result.reason, Some("test denial".to_string()));
}

#[tokio::test]
async fn test_engine_fail_closed_on_invalid_policy() {
    let tmp = TempDir::new().unwrap();
    let policy = create_test_policy(
        &tmp,
        r#"
# Invalid policy - no evaluate function
def other():
    return "allow"
"#,
    )
    .await;

    // Policy loads successfully but evaluation fails at runtime
    let engine = PolicyEngine::new(&policy).await.unwrap();
    let result = engine.evaluate("test", &serde_json::json!({}), None).await;

    // Should fail closed (deny) when evaluate function not found
    assert_eq!(result.verdict, Verdict::Deny);
    assert!(result.reason.unwrap().to_lowercase().contains("evaluate"));
}

#[tokio::test]
async fn test_engine_fail_closed_on_runtime_error() {
    let tmp = TempDir::new().unwrap();
    let policy = create_test_policy(
        &tmp,
        r#"
def evaluate(tool, args, context):
    # This will cause a runtime error
    return undefined_variable
"#,
    )
    .await;

    let engine = PolicyEngine::new(&policy).await.unwrap();
    let result = engine.evaluate("test", &serde_json::json!({}), None).await;

    // Should fail closed (deny) on runtime error
    assert_eq!(result.verdict, Verdict::Deny);
    assert!(result.reason.unwrap().contains("error"));
}

#[tokio::test]
async fn test_domain_extraction_from_url() {
    let args = serde_json::json!({"url": "https://example.com/path"});
    let domain = PolicyEngine::extract_domain(&args);
    assert_eq!(domain, Some("example.com".to_string()));
}

#[tokio::test]
async fn test_domain_extraction_from_domain_field() {
    let args = serde_json::json!({"domain": "example.org"});
    let domain = PolicyEngine::extract_domain(&args);
    assert_eq!(domain, Some("example.org".to_string()));
}

#[tokio::test]
async fn test_domain_extraction_no_domain() {
    let args = serde_json::json!({"command": "ls -la"});
    let domain = PolicyEngine::extract_domain(&args);
    assert_eq!(domain, None);
}

#[tokio::test]
async fn test_agent_config_loading() {
    let tmp = TempDir::new().unwrap();
    let policy = create_test_policy(
        &tmp,
        r#"
def evaluate(tool, args, context):
    return "allow"
"#,
    )
    .await;

    let engine = PolicyEngine::new(&policy).await.unwrap();

    let configs = vec![AgentPolicyConfig {
        agent_id: "test-agent".to_string(),
        allowed_domains: vec!["safe.com".to_string()],
        denied_domains: vec!["evil.com".to_string()],
        domain_list_sources: vec![],
    }];

    engine.set_agent_configs(configs).await;

    // Test that agent context is passed to policy
    let result = engine
        .evaluate(
            "browser",
            &serde_json::json!({"url": "https://example.com"}),
            Some("test-agent"),
        )
        .await;

    assert_eq!(result.verdict, Verdict::Allow);
}
