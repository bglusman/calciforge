//! Integration tests for the NZC Clash policy approval flow.
//!
//! These tests validate the interaction between:
//!   1. `StarlarkPolicy` evaluation (Review verdict)
//!   2. `run_tool_call_loop` → `ReviewPendingError` suspension
//!   3. The `PendingApprovals` channel used by the gateway
//!   4. Post-approval continuation vs denial path
//!
//! # Strategy
//!
//! Spinning up a full `AppState` (gateway + HTTP + real LLM provider) requires
//! a live provider. Instead, these tests exercise the policy evaluation path
//! directly, and use the mock provider infrastructure to simulate the agent
//! loop path where the policy fires.
//!
//! The key assertions:
//! - A Review verdict fires when `rm -rf /tmp/x` is submitted for "brian"
//! - The verdict carries the correct command string in its reason
//! - An always-deny (root wipe) bypasses the review path entirely
//! - Policy evaluation is correct before and after fix (normalize)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use clash::{ClashPolicy, PolicyContext, PolicyVerdict, StarlarkPolicy};
use nonzeroclaw::agent::agent::Agent;
use nonzeroclaw::agent::dispatcher::NativeToolDispatcher;
use nonzeroclaw::config::MemoryConfig;
use nonzeroclaw::memory;
use nonzeroclaw::memory::Memory;
use nonzeroclaw::observability::NoopObserver;
use nonzeroclaw::providers::{ChatRequest, ChatResponse, Provider, ToolCall};
use nonzeroclaw::tools::{Tool, ToolResult};

// Suppress dead code warnings for test helpers that may not be used in all configurations
#[allow(dead_code)]

// ─────────────────────────────────────────────────────────────────────────────
// Policy loading helper
// ─────────────────────────────────────────────────────────────────────────────

/// Load the real NZC policy from the clash crate's examples directory.
/// This is the same policy deployed to .210 — tests run against the real rules.
fn nzc_policy() -> Arc<dyn ClashPolicy> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clash/examples/policy.star");
    Arc::new(StarlarkPolicy::load_with_profiles(path))
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock Provider
// ─────────────────────────────────────────────────────────────────────────────

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("Task completed.".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Policy-level approval flow tests (no LLM needed)
// ─────────────────────────────────────────────────────────────────────────────

/// Core: the policy fires Review for a standard rm -rf command from an admin identity.
/// This is what causes ReviewPendingError in the agent loop when pending_approvals is set.
#[test]
fn policy_fires_review_for_admin_rm_rf() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
        .with_command("rm -rf /tmp/test-approval-integration");
    let verdict = policy.evaluate("tool:shell", &ctx);

    match verdict {
        PolicyVerdict::Review(reason) => {
            // The reason should contain the command
            assert!(
                reason.contains("rm -rf /tmp/test-approval-integration"),
                "Review reason should contain the command, got: {:?}",
                reason
            );
            assert!(
                reason.contains("requires approval"),
                "Review reason should mention approval, got: {:?}",
                reason
            );
        }
        other => panic!(
            "Expected Review for 'rm -rf /tmp/test-approval-integration' by brian, got: allow={}, deny={}",
            matches!(other, PolicyVerdict::Allow),
            matches!(other, PolicyVerdict::Deny(_))
        ),
    }
}

/// The denial path: root wipe is always-deny with no approval possible.
/// When the policy returns Deny, the agent loop does NOT create a PendingApproval
/// entry — it just injects a denial result and continues.
#[test]
fn policy_always_deny_for_root_wipe() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
        .with_command("rm -rf /");
    let verdict = policy.evaluate("tool:shell", &ctx);

    match verdict {
        PolicyVerdict::Deny(reason) => {
            assert!(
                reason.contains("root filesystem wipe") || reason.contains("catastrophic"),
                "Deny reason should mention the reason, got: {:?}",
                reason
            );
        }
        other => panic!(
            "Expected Deny for root wipe, got: allow={}, review={}",
            matches!(other, PolicyVerdict::Allow),
            matches!(other, PolicyVerdict::Review(_))
        ),
    }
}

/// Approval flow — simulated: send approved=true signal through a oneshot channel.
/// This tests the channel mechanics without needing a full HTTP stack or LLM.
#[tokio::test]
async fn approval_flow_approve_signal_passes_through_channel() {
    // Create a pending approval entry as the gateway would
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<bool>();

    let pending_approvals: nonzeroclaw::gateway::PendingApprovals =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    pending_approvals.lock().await.insert(
        request_id.clone(),
        nonzeroclaw::gateway::PendingApproval {
            reason: "Command requires approval: rm -rf /tmp/test-approval-integration".into(),
            command: "rm -rf /tmp/test-approval-integration".into(),
            result_tx,
        },
    );

    // Simulate the /webhook/approve handler: take the entry, send approved=true
    let pending = {
        let mut store = pending_approvals.lock().await;
        store.remove(&request_id)
    };
    assert!(pending.is_some(), "PendingApproval should exist");
    let pending = pending.unwrap();
    assert_eq!(pending.command, "rm -rf /tmp/test-approval-integration");

    // Send approval
    pending.result_tx.send(true).unwrap();

    // The agent loop would be waiting on result_rx — verify it receives the signal
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        result_rx,
    )
    .await
    .expect("Channel receive timed out")
    .expect("Oneshot sender dropped unexpectedly");

    assert!(decision, "Expected approved=true from channel");
}

/// Approval flow — denial path: send approved=false.
#[tokio::test]
async fn approval_flow_deny_signal_passes_through_channel() {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<bool>();
    let pending = nonzeroclaw::gateway::PendingApproval {
        reason: "Command requires approval: rm -rf /tmp/x".into(),
        command: "rm -rf /tmp/x".into(),
        result_tx,
    };

    // Simulate denial
    pending.result_tx.send(false).unwrap();

    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        result_rx,
    )
    .await
    .expect("Channel receive timed out")
    .expect("Oneshot sender dropped");

    assert!(!decision, "Expected approved=false (denial) from channel");
}

/// Verify that PendingApprovals is correctly absent after removal (approval consumed).
#[tokio::test]
async fn approval_flow_consumed_entry_is_removed() {
    let request_id = "test-req-id-consumed".to_string();
    let (result_tx, _result_rx) = tokio::sync::oneshot::channel::<bool>();

    let pending_approvals: nonzeroclaw::gateway::PendingApprovals =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    pending_approvals.lock().await.insert(
        request_id.clone(),
        nonzeroclaw::gateway::PendingApproval {
            reason: "test".into(),
            command: "rm -rf /tmp/x".into(),
            result_tx,
        },
    );

    // Consume the approval
    let consumed = pending_approvals.lock().await.remove(&request_id);
    assert!(consumed.is_some());

    // Should be gone now
    let second_attempt = pending_approvals.lock().await.remove(&request_id);
    assert!(second_attempt.is_none(), "Approval should be consumed (removed) after first take");
}

/// Verify PendingResults store correctly persists and retrieves responses.
#[tokio::test]
async fn pending_results_store_and_retrieve() {
    let request_id = "test-result-id".to_string();
    let pending_results: nonzeroclaw::gateway::PendingResults =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // Simulate the background continuation task storing its result
    pending_results.lock().await.insert(
        request_id.clone(),
        nonzeroclaw::gateway::PendingResult {
            response: "Command executed successfully.\nOutput:\nfile deleted".into(),
        },
    );

    // Simulate the poll endpoint consuming it
    let result = pending_results.lock().await.remove(&request_id);
    assert!(result.is_some(), "Result should be retrievable");
    let result = result.unwrap();
    assert!(
        result.response.contains("executed successfully"),
        "Response should contain execution output, got: {:?}",
        result.response
    );

    // After consumption, should be gone
    let second = pending_results.lock().await.remove(&request_id);
    assert!(second.is_none(), "Result should be consumed after retrieval");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Policy integration with agent loop (no pending_approvals → allow through)
// ─────────────────────────────────────────────────────────────────────────────

/// In CLI/test context (no pending_approvals store), Review verdicts fall through
/// to allow execution. This matches the existing behavior: clash only suspends
/// in gateway context where pending_approvals is provided.
///
/// This test uses the Agent with a mock provider and a real policy to confirm
/// that the agent loop doesn't crash when Review fires without a pending_approvals store.
#[tokio::test]
async fn agent_loop_review_falls_through_in_cli_context() {
    // Mock provider: responds with a shell tool call, then a final text response
    let tool_call_response = nonzeroclaw::providers::ChatResponse {
        text: None,
        tool_calls: vec![ToolCall {
            id: "tc-1".into(),
            name: "shell".into(),
            arguments: json!({"command": "rm -rf /tmp/clash-test-dir"}).to_string(),
        }],
        usage: None,
        reasoning_content: None,
    };
    let final_response = nonzeroclaw::providers::ChatResponse {
        text: Some("Done — removed the directory.".into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    };

    let provider: Box<dyn Provider> = Box::new(ScriptedProvider::new(vec![tool_call_response, final_response]));

    // Mock memory
    let mem_config = MemoryConfig { backend: "none".into(), ..Default::default() };
    let mem: Arc<dyn Memory> = Arc::from(
        memory::create_memory(&mem_config, &std::path::PathBuf::from("/tmp"), None)
            .expect("memory creation failed")
    );

    // Mock shell tool that records what was called
    let called_cmds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let called_cmds_clone = called_cmds.clone();

    struct RecordingShellTool {
        called: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for RecordingShellTool {
        fn name(&self) -> &str { "shell" }
        fn description(&self) -> &str { "Execute a shell command" }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
        }
        async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
            let cmd = args["command"].as_str().unwrap_or("").to_string();
            self.called.lock().unwrap().push(cmd);
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(RecordingShellTool { called: called_cmds_clone }),
    ];

    let observer: Arc<dyn nonzeroclaw::observability::Observer> = Arc::new(NoopObserver);

    let mut agent = Agent::builder()
        .provider(provider)
        .tools(tools)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .observer(observer)
        .memory(mem)
        .workspace_dir(std::env::temp_dir())
        .build()
        .expect("Agent build failed");

    // Run the agent turn — the policy will fire Review for rm -rf but in CLI context
    // (no pending_approvals) it falls through and the shell tool executes.
    let result: Result<String> = agent.turn("Remove the test directory").await;

    // In CLI context, the agent loop allows Review verdicts through (fallback behavior).
    // The tool SHOULD have been called.
    assert!(result.is_ok(), "Agent turn should not error in CLI context: {:?}", result);
    let called = called_cmds.lock().unwrap();
    assert!(
        !called.is_empty(),
        "Shell tool should have been called (Review falls through in CLI context)"
    );
    assert_eq!(
        called[0], "rm -rf /tmp/clash-test-dir",
        "Shell tool should have received the exact command"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Policy evaluation: normalized whitespace (fix regression)
// ─────────────────────────────────────────────────────────────────────────────

/// Regression test: verify that double-space evasion is caught after the fix.
/// Before the fix: "rm  -rf /tmp/foo" → Allow (bypass)
/// After the fix: "rm  -rf /tmp/foo" → Review (caught by normalize())
#[test]
fn policy_catches_double_space_rm_rf_after_fix() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
        .with_command("rm  -rf /tmp/foo");
    let verdict = policy.evaluate("tool:shell", &ctx);
    assert!(
        matches!(verdict, PolicyVerdict::Review(_)),
        "After normalize() fix, double-space 'rm  -rf' should be caught as Review"
    );
}

/// Regression test: verify that tab evasion is caught after the fix.
/// Before the fix: "rm\t-rf /tmp/foo" → Allow (bypass)
/// After the fix: "rm\t-rf /tmp/foo" → Review (caught by normalize())
#[test]
fn policy_catches_tab_rm_rf_after_fix() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
        .with_command("rm\t-rf /tmp/foo");
    let verdict = policy.evaluate("tool:shell", &ctx);
    assert!(
        matches!(verdict, PolicyVerdict::Review(_)),
        "After normalize() fix, tab 'rm\\t-rf' should be caught as Review"
    );
}

/// Regression test: verify that double-space in zfs destroy -r is caught after fix.
/// Before the fix: "zfs destroy  -r pool" → Review (downgraded from always-deny)
/// After the fix: "zfs destroy  -r pool" → Deny (correctly caught by always-deny)
#[test]
fn policy_catches_double_space_zfs_destroy_r_after_fix() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
        .with_command("zfs destroy  -r pool");
    let verdict = policy.evaluate("tool:shell", &ctx);
    assert!(
        matches!(verdict, PolicyVerdict::Deny(_)),
        "After normalize() fix, 'zfs destroy  -r pool' should be always-deny"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Research profile: policy correctly restricts non-allowed commands
// ─────────────────────────────────────────────────────────────────────────────

/// Research identity "renee" can only run commands in RESEARCH_ALLOWED_COMMANDS.
/// The policy check runs AFTER the review/deny gates, so safe commands that are
/// in the allowed list get through but disallowed commands get denied.
#[test]
fn research_policy_allowed_commands_pass() {
    let policy = nzc_policy();
    let allowed_cmds = &[
        "ls /tmp",
        "cat /etc/hosts",
        "grep -r pattern /tmp",
        "python3 script.py",
        "curl http://example.com",
        "wget http://example.com/file",
        "echo hello world",
        "pwd",
        "df -h",
    ];

    for cmd in allowed_cmds {
        let ctx = PolicyContext::new("renee", "nzc", "tool:shell").with_command(cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Allow),
            "Expected Allow for renee command {:?}, got non-Allow",
            cmd
        );
    }
}

#[test]
fn research_policy_disallowed_commands_denied() {
    let policy = nzc_policy();
    let denied_cmds = &[
        "rm /tmp/foo",
        "sudo ls",
        "apt install foo",
        "chmod 777 /tmp/x",
        "mv /tmp/a /tmp/b",
        "cp /etc/shadow /tmp/shadow",
    ];

    for cmd in denied_cmds {
        let ctx = PolicyContext::new("renee", "nzc", "tool:shell").with_command(cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Deny(_)),
            "Expected Deny for renee command {:?}, got non-Deny",
            cmd
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. File-write tests with real policy
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn file_write_lucien_policy_dir_denied() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("lucien", "nzc", "tool:file_write")
        .with_path("/etc/nonzeroclaw/workspace/.clash/policy.star");
    let verdict = policy.evaluate("tool:file_write", &ctx);
    assert!(
        matches!(verdict, PolicyVerdict::Deny(_)),
        "Lucien writing to .clash/policy.star should be denied"
    );
}

#[test]
fn file_write_lucien_other_path_review() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("lucien", "nzc", "tool:file_write")
        .with_path("/etc/nonzeroclaw/workspace/config.toml");
    let verdict = policy.evaluate("tool:file_write", &ctx);
    assert!(
        matches!(verdict, PolicyVerdict::Review(_)),
        "Lucien writing to config.toml should require review"
    );
}

#[test]
fn file_write_brian_any_path_allowed() {
    let policy = nzc_policy();
    let paths = &["/tmp/foo", "/etc/nonzeroclaw/workspace/config.toml", "/root/test.md"];
    for path in paths {
        let ctx = PolicyContext::new("brian", "nzc", "tool:file_write").with_path(path);
        let verdict = policy.evaluate("tool:file_write", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Allow),
            "Brian writing to {:?} should be allowed (not lucien, not research)",
            path
        );
    }
}

#[test]
fn file_write_renee_any_path_review() {
    let policy = nzc_policy();
    let paths = &["/tmp/foo", "/home/renee/notes.md"];
    for path in paths {
        let ctx = PolicyContext::new("renee", "nzc", "tool:file_write").with_path(path);
        let verdict = policy.evaluate("tool:file_write", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Review(_)),
            "Renee writing to {:?} should require review",
            path
        );
    }
}

// Temporary debug test: print policy verdicts for failing cases. Remove after debugging.
#[test]
fn debug_policy_verdicts() {
    let policy = nzc_policy();

    let cases = vec![
        ("lucien", "tool:file_write", "/etc/nonzeroclaw/workspace/.clash/policy.star"),
        ("lucien", "tool:file_write", "/etc/nonzeroclaw/workspace/config.toml"),
        ("renee", "tool:file_write", "/tmp/foo"),
        ("renee", "tool:shell", "rm /tmp/foo"),
    ];

    for (identity, tool, subject) in cases {
        let ctx = if tool == "tool:shell" {
            PolicyContext::new(identity, "nzc", tool).with_command(subject)
        } else {
            PolicyContext::new(identity, "nzc", tool).with_path(subject)
        };
        let verdict = policy.evaluate(tool, &ctx);
        match verdict {
            PolicyVerdict::Allow => println!("CASE: identity={} tool={} subject={} => Allow", identity, tool, subject),
            PolicyVerdict::Review(reason) => println!("CASE: identity={} tool={} subject={} => Review: {}", identity, tool, subject, reason),
            PolicyVerdict::Deny(reason) => println!("CASE: identity={} tool={} subject={} => Deny: {}", identity, tool, subject, reason),
        }
    }
}
