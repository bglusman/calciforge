//! Clash policy trait contracts for NonZeroClaw.
//!
//! Phase 1: Trait contracts + no-op (permissive) implementation.
//! Phase 2: Real Starlark evaluation engine (`StarlarkPolicy`).
//!
//! # Design
//!
//! Clash policies are evaluated on every agent action before execution.
//! The router (ZeroClawed) or the agent runtime calls `ClashPolicy::evaluate`
//! with the action name and its context. If the verdict is `Deny` or `Review`,
//! the agent must not proceed.

mod starlark_policy;

#[cfg(test)]
mod policy_tests;

#[cfg(test)]
mod adversarial_tests;

#[cfg(test)]
mod policy_proptest;

pub use starlark_policy::StarlarkPolicy;

use std::collections::HashMap;

/// Context provided to a [`ClashPolicy`] for evaluation.
pub struct PolicyContext {
    /// The identity name of the issuing agent.
    pub identity: String,
    /// The agent instance identifier (e.g. agent ID, UUID).
    pub agent: String,
    /// The action being requested (e.g. "tool:shell", "tool:web_fetch").
    pub action: String,
    /// Extra context for the policy engine. For shell tool calls, includes:
    ///   "command" => the full shell command string being executed
    pub extra: HashMap<String, String>,
}

impl PolicyContext {
    /// Create a new `PolicyContext` with empty extra fields.
    pub fn new(identity: &str, agent: &str, action: &str) -> Self {
        Self {
            identity: identity.to_string(),
            agent: agent.to_string(),
            action: action.to_string(),
            extra: HashMap::new(),
        }
    }

    /// Attach the shell command string to extra context.
    pub fn with_command(mut self, command: &str) -> Self {
        self.extra.insert("command".to_string(), command.to_string());
        self
    }

    /// Attach the file path to extra context (file_read / file_write calls).
    pub fn with_path(mut self, path: &str) -> Self {
        self.extra.insert("path".to_string(), path.to_string());
        self
    }
}

/// Verdict returned by a [`ClashPolicy`].
pub enum PolicyVerdict {
    /// The action is permitted; proceed normally.
    Allow,
    /// The action is denied; do not execute. Carry the reason string.
    Deny(String),
    /// The action requires human review before proceeding. Carry the reason string.
    Review(String),
}

/// Trait implemented by all clash policy engines.
///
/// # Implementing a Policy
///
/// ```rust
/// use clash::{ClashPolicy, PolicyContext, PolicyVerdict};
///
/// struct AlwaysAllow;
///
/// impl ClashPolicy for AlwaysAllow {
///     fn evaluate(&self, action: &str, context: &PolicyContext) -> PolicyVerdict {
///         PolicyVerdict::Allow
///     }
/// }
/// ```
pub trait ClashPolicy: Send + Sync {
    /// Evaluate whether `action` in `context` is permitted.
    fn evaluate(&self, action: &str, context: &PolicyContext) -> PolicyVerdict;
}

/// No-op (permissive) clash policy.
///
/// Allows everything. This is the default when no policy file is found.
///
/// # Safety
///
/// This policy performs no enforcement. It exists as a type-safe placeholder
/// so the plumbing is wired up before the real enforcement logic is implemented.
pub struct PermissivePolicy;

impl ClashPolicy for PermissivePolicy {
    /// Always returns [`PolicyVerdict::Allow`] for any action and context.
    fn evaluate(&self, _action: &str, _context: &PolicyContext) -> PolicyVerdict {
        PolicyVerdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_policy_allows_all() {
        let policy = PermissivePolicy;
        let ctx = PolicyContext::new("nonzeroclaw", "agent-1", "tool:shell");

        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(matches!(verdict, PolicyVerdict::Allow));
    }

    #[test]
    fn permissive_policy_allows_destructive_actions() {
        let policy = PermissivePolicy;
        let ctx = PolicyContext::new("nonzeroclaw", "agent-1", "tool:file_write");

        let verdict = policy.evaluate("tool:file_write", &ctx);
        assert!(matches!(verdict, PolicyVerdict::Allow));
    }

    #[test]
    fn starlark_policy_missing_file_falls_back_to_permissive() {
        let policy =
            StarlarkPolicy::load(std::path::PathBuf::from("/nonexistent/path/policy.star"));
        let ctx = PolicyContext::new("alice", "nzc", "tool:shell");
        // Must allow (permissive fallback)
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn starlark_policy_evaluates_deny() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and identity not in ["brian", "librarian"]:
        return "deny:shell not permitted for " + identity
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("alice", "nzc", "tool:shell");
        let verdict = policy.evaluate("tool:shell", &ctx);
        match verdict {
            PolicyVerdict::Deny(reason) => {
                assert!(reason.contains("shell not permitted for alice"), "got: {reason}");
            }
            other => panic!("Expected Deny, got {:?}", matches!(other, PolicyVerdict::Allow)),
        }
    }

    #[test]
    fn starlark_policy_evaluates_allow() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("alice", "nzc", "tool:shell");
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn starlark_policy_evaluates_review() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:file_write":
        return "review:file write requires approval"
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("alice", "nzc", "tool:file_write");
        let verdict = policy.evaluate("tool:file_write", &ctx);
        match verdict {
            PolicyVerdict::Review(reason) => {
                assert!(reason.contains("file write requires approval"), "got: {reason}");
            }
            other => panic!("Expected Review, got {:?}", matches!(other, PolicyVerdict::Allow)),
        }
    }

    #[test]
    fn starlark_policy_allows_privileged_shell() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and identity not in ["brian", "librarian"]:
        return "deny:shell not permitted"
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("brian", "nzc", "tool:shell");
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn starlark_policy_fails_open_on_error() {
        // Script that raises an error during evaluation
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    fail("intentional error")
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("alice", "nzc", "tool:shell");
        // On Starlark error, must fail-open (Allow)
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn starlark_policy_command_aware_deny() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and "rm -rf /" in command:
        return "deny:catastrophic command blocked"
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
            .with_command("rm -rf /");
        let verdict = policy.evaluate("tool:shell", &ctx);
        match verdict {
            PolicyVerdict::Deny(reason) => {
                assert!(reason.contains("catastrophic command blocked"), "got: {reason}");
            }
            other => panic!("Expected Deny, got Allow={}", matches!(other, PolicyVerdict::Allow)),
        }
    }

    #[test]
    fn starlark_policy_command_aware_allow_safe_command() {
        let script = r#"
def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:shell" and "rm -rf /" in command:
        return "deny:catastrophic command blocked"
    return "allow"
"#;
        let policy = StarlarkPolicy::from_source("<test>", script);
        let ctx = PolicyContext::new("brian", "nzc", "tool:shell")
            .with_command("ls /etc");
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    // ── Lucien-specific policy tests ─────────────────────────────────────

    /// Load the real example policy.star with profile chain so we're testing actual deployed logic,
    /// not a hand-written inline stub. Uses load_with_profiles() so profiles/ dir is enabled.
    fn load_example_policy() -> StarlarkPolicy {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/policy.star");
        StarlarkPolicy::load_with_profiles(path)
    }

    #[test]
    fn lucien_cannot_write_to_clash_dir() {
        let policy = load_example_policy();
        let ctx = PolicyContext::new("lucien", "nzc", "tool:file_write")
            .with_path("/etc/nonzeroclaw/workspace/.clash/policy.star");
        let verdict = policy.evaluate("tool:file_write", &ctx);
        match verdict {
            PolicyVerdict::Deny(reason) => {
                assert!(reason.contains("Protected file"), "got: {reason}");
            }
            other => panic!("Expected Deny, got Allow={}", matches!(other, PolicyVerdict::Allow)),
        }
    }

    /// Non-protected paths for Lucien now return Allow (not Review).
    /// The new model only blocks the 5 specific PROTECTED_FILES.
    #[test]
    fn lucien_non_protected_file_write_is_allow() {
        let policy = load_example_policy();
        let ctx = PolicyContext::new("lucien", "nzc", "tool:file_write")
            .with_path("/etc/nonzeroclaw/workspace/notes.md");
        let verdict = policy.evaluate("tool:file_write", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Allow),
            "Expected Allow for lucien writing to non-protected path, got non-Allow"
        );
    }

    #[test]
    fn lucien_can_read_clash_dir() {
        let policy = load_example_policy();
        // file_read is not restricted for Lucien — he needs to read the policy
        let ctx = PolicyContext::new("lucien", "nzc", "tool:file_read")
            .with_path("/etc/nonzeroclaw/workspace/.clash/policy.star");
        assert!(matches!(policy.evaluate("tool:file_read", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn lucien_can_run_safe_shell_commands() {
        let policy = load_example_policy();
        let ctx = PolicyContext::new("lucien", "nzc", "tool:shell")
            .with_command("systemctl status nonzeroclaw");
        assert!(matches!(policy.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn lucien_catastrophic_shell_denied() {
        let policy = load_example_policy();
        let ctx = PolicyContext::new("lucien", "nzc", "tool:shell")
            .with_command("rm -rf /");
        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(matches!(verdict, PolicyVerdict::Deny(_)));
    }
}
