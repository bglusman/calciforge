//! Starlark evaluator implementation
//!
//! Handles the actual Starlark execution using the starlark crate.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use starlark::{
    collections::SmallMap,
    environment::{Globals, Module},
    eval::Evaluator,
    syntax::{AstModule, Dialect},
    values::{dict::Dict, Value as StarlarkValue},
};
use std::path::Path;
use tracing::{debug, error, info};

use super::{PolicyResult, Verdict};

/// Wrapper around Starlark evaluation
pub struct PolicyEvaluator {
    ast: AstModule,
    globals: Globals,
}

impl PolicyEvaluator {
    /// Create a new evaluator by loading policy from disk
    pub async fn new(policy_path: &Path) -> Result<Self> {
        if !policy_path.exists() {
            return Err(anyhow!("Policy file not found: {}", policy_path.display()));
        }

        let source = tokio::fs::read_to_string(policy_path)
            .await
            .with_context(|| format!("Failed to read policy: {}", policy_path.display()))?;

        // Parse and validate the Starlark source
        let ast = Self::parse(&source)?;
        let globals = Globals::standard();

        info!("Starlark policy loaded: {} bytes", source.len());
        Ok(Self { ast, globals })
    }

    /// Parse Starlark source into an AST
    fn parse(source: &str) -> Result<AstModule> {
        let dialect = Dialect::Standard;
        let ast = AstModule::parse("policy.star", source.to_owned(), &dialect)
            .map_err(|e| anyhow!("Starlark parse error: {}", e))?;

        Ok(ast)
    }

    /// Convert serde_json::Value to Starlark Value
    fn json_to_starlark<'v>(value: &Value, heap: &'v starlark::values::Heap) -> StarlarkValue<'v> {
        match value {
            Value::Null => StarlarkValue::new_none(),
            Value::Bool(b) => StarlarkValue::new_bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    heap.alloc(i)
                } else if let Some(f) = n.as_f64() {
                    heap.alloc(f)
                } else {
                    heap.alloc(n.to_string())
                }
            }
            Value::String(s) => heap.alloc(s.as_str()),
            Value::Array(arr) => {
                let values: Vec<StarlarkValue<'v>> = arr
                    .iter()
                    .map(|v| Self::json_to_starlark(v, heap))
                    .collect();
                heap.alloc(values)
            }
            Value::Object(map) => {
                // Build dict using Starlark Dict type via SmallMap
                let mut small_map = SmallMap::with_capacity(map.len());
                for (k, v) in map.iter() {
                    let key = heap.alloc(k.as_str());
                    let val = Self::json_to_starlark(v, heap);
                    small_map.insert_hashed(
                        key.get_hashed().expect("string keys are always hashable"),
                        val,
                    );
                }
                heap.alloc(Dict::new(small_map))
            }
        }
    }

    /// Parse the result from Starlark evaluation
    fn parse_result(result: StarlarkValue) -> Result<PolicyResult> {
        // Check if result is a string
        if let Some(s) = result.unpack_str() {
            return Self::verdict_from_string(s, None);
        }

        // Try to get as dict to extract verdict and reason
        // Convert the value to a JSON-like representation for parsing
        let result_json: serde_json::Value = serde_json::from_str(&result.to_str())
            .map_err(|_| anyhow!("Policy result must be a string or dict"))?;

        if let Some(obj) = result_json.as_object() {
            if let Some(v) = obj.get("verdict").and_then(|v| v.as_str()) {
                let reason = obj
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
                return Self::verdict_from_string(v, reason);
            }
        }

        Err(anyhow!(
            "Policy evaluate() must return a string or dict with 'verdict' key"
        ))
    }

    /// Convert a string verdict to PolicyResult
    fn verdict_from_string(verdict: &str, reason: Option<String>) -> Result<PolicyResult> {
        let v = match verdict {
            "allow" => Verdict::Allow,
            "review" => Verdict::Review,
            "deny" => Verdict::Deny,
            _ => {
                return Err(anyhow!(
                    "Invalid verdict: {}. Must be allow/review/deny",
                    verdict
                ))
            }
        };

        Ok(PolicyResult { verdict: v, reason })
    }

    /// Evaluate a tool call against the policy
    pub async fn evaluate(
        &self,
        tool: &str,
        args: &Value,
        context: Option<&Value>,
    ) -> Result<PolicyResult> {
        debug!(tool = %tool, "Evaluating against Starlark policy");

        // Create a fresh module for this evaluation
        let module = Module::new();

        // Create evaluator and execute the module to define the evaluate function
        let mut eval = Evaluator::new(&module);

        // First, evaluate the module to define the evaluate function
        // Note: eval_module consumes the AST, so we clone it for each evaluation
        let _ = eval
            .eval_module(self.ast.clone(), &self.globals)
            .map_err(|e| {
                error!(error = %e, "Failed to evaluate policy module");
                anyhow!("Policy module evaluation error: {}", e)
            })?;

        // Get the heap for allocating arguments
        let heap = module.heap();

        // Convert arguments to Starlark values
        let tool_val = heap.alloc(tool);
        let args_val = Self::json_to_starlark(args, heap);
        let context_val = context
            .map(|c| Self::json_to_starlark(c, heap))
            .unwrap_or_else(StarlarkValue::new_none);

        // Get the evaluate function from the module
        let evaluate_fn = module
            .get("evaluate")
            .ok_or_else(|| anyhow!("Policy must define an 'evaluate' function"))?;

        // Call the evaluate function
        let result = eval
            .eval_function(evaluate_fn, &[tool_val, args_val, context_val], &[])
            .map_err(|e| {
                error!(error = %e, "Starlark evaluation failed");
                anyhow!("Policy evaluation error: {}", e)
            })?;

        Self::parse_result(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    async fn create_test_policy(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let policy_path = temp_dir.path().join("policy.star");
        let mut file = std::fs::File::create(&policy_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        (temp_dir, policy_path)
    }

    #[tokio::test]
    async fn test_load_valid_policy() {
        let (_temp_dir, policy_path) =
            create_test_policy("def evaluate(tool, args, context):\n    return 'allow'\n").await;

        let evaluator = PolicyEvaluator::new(&policy_path).await;
        assert!(evaluator.is_ok());
    }

    #[tokio::test]
    async fn test_load_missing_policy() {
        let policy_path = std::path::PathBuf::from("/nonexistent/policy.star");
        let result = PolicyEvaluator::new(&policy_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_evaluate_allow() {
        let (_temp_dir, policy_path) =
            create_test_policy("def evaluate(tool, args, context):\n    return 'allow'\n").await;

        let evaluator = PolicyEvaluator::new(&policy_path).await.unwrap();
        let result = evaluator.evaluate("test", &json!({}), None).await.unwrap();

        assert_eq!(result.verdict, Verdict::Allow);
        assert!(result.reason.is_none());
    }

    #[tokio::test]
    async fn test_evaluate_deny() {
        let (_temp_dir, policy_path) =
            create_test_policy("def evaluate(tool, args, context):\n    return 'deny'\n").await;

        let evaluator = PolicyEvaluator::new(&policy_path).await.unwrap();
        let result = evaluator.evaluate("test", &json!({}), None).await.unwrap();

        assert_eq!(result.verdict, Verdict::Deny);
    }

    #[tokio::test]
    async fn test_evaluate_review() {
        let (_temp_dir, policy_path) = create_test_policy(
            "def evaluate(tool, args, context):\n    return {'verdict': 'review', 'reason': 'Needs approval'}\n"
        ).await;

        let evaluator = PolicyEvaluator::new(&policy_path).await.unwrap();
        let result = evaluator.evaluate("test", &json!({}), None).await.unwrap();

        assert_eq!(result.verdict, Verdict::Review);
        assert_eq!(result.reason, Some("Needs approval".to_string()));
    }

    #[tokio::test]
    async fn test_evaluate_with_tool_arg() {
        let (_temp_dir, policy_path) = create_test_policy(
            r#"def evaluate(tool, args, context):
    if tool == "gateway":
        return "review"
    return "allow"
"#,
        )
        .await;

        let evaluator = PolicyEvaluator::new(&policy_path).await.unwrap();

        let result = evaluator
            .evaluate("gateway", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(result.verdict, Verdict::Review);

        let result = evaluator.evaluate("shell", &json!({}), None).await.unwrap();
        assert_eq!(result.verdict, Verdict::Allow);
    }
}
