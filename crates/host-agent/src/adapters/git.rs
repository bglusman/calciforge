//! GitAdapter — safe git operations on allowlisted repositories.
//!
//! Supported operations (args[0]):
//!   - `status`   – git status (read-only)
//!   - `fetch`    – git fetch origin
//!   - `pull`     – git pull
//!   - `checkout` – git checkout <branch> (args[1] = branch name)
//!   - `log`      – git log --oneline -10 (read-only)
//!
//! The repository path (resource) must:
//!   1. Be in the configured repo allowlist (`git.allowed_repos` in config).
//!   2. Be an absolute path with no `..` components.
//!   3. Be an actual directory on disk.
//!
//! No shell interpolation: all arguments are passed as separate argv tokens.
//!
//! HostOp mapping:
//! ```json
//! { "kind": "git", "resource": "/srv/myapp", "args": ["status"] }
//! { "kind": "git", "resource": "/srv/myapp", "args": ["checkout", "main"] }
//! ```

use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use tokio::process::Command;
use tracing::info;

use crate::adapters::{Adapter, ExecutionResult, HostOp, PolicyDecision};
use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::AppState;

const GIT_BIN: &str = "/usr/bin/git";

/// Branch name regex: alphanumeric, hyphen, underscore, dot, slash.
static BRANCH_RE: OnceLock<Regex> = OnceLock::new();
fn branch_re() -> &'static Regex {
    BRANCH_RE.get_or_init(|| {
        Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_\-./]*$").expect("BRANCH_RE valid")
    })
}

/// Validate a git branch/ref name.
pub fn is_valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && !name.starts_with('-')
        && !name.ends_with('/')
        && branch_re().is_match(name)
}

/// Validate a repo path:
///   - Must be absolute
///   - Must not contain `..` components
///   - Must exist on disk as a directory
pub fn is_valid_repo_path(path: &str) -> bool {
    let p = Path::new(path);
    if !p.is_absolute() {
        return false;
    }
    // Reject any path component that is ".."
    for component in p.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir) {
            return false;
        }
    }
    // Must exist as a directory
    p.is_dir()
}

pub struct GitAdapter;

impl GitAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for GitAdapter {
    fn kind(&self) -> &'static str {
        "git"
    }

    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError> {
        let command = op
            .command()
            .ok_or_else(|| AppError::Internal("git: args[0] (command) is required".into()))?;

        // Validate supported commands
        match command {
            "status" | "fetch" | "pull" | "checkout" | "log" => {}
            other => {
                return Ok(PolicyDecision::Deny {
                    reason: format!("GitAdapter: unsupported command '{other}'"),
                });
            }
        }

        // Validate repository path
        let repo_path = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("git: resource (repo path) is required".into()))?;

        if !is_valid_repo_path(repo_path) {
            return Ok(PolicyDecision::Deny {
                reason: format!(
                    "GitAdapter: invalid or non-existent repo path '{repo_path}' \
                    (must be absolute, no '..', must exist)"
                ),
            });
        }

        // Check repo allowlist from config
        let config = state.config.get().await;
        if let Some(ref git_cfg) = config.git {
            if !git_cfg.allowed_repos.is_empty() {
                let allowed = git_cfg
                    .allowed_repos
                    .iter()
                    .any(|allowed| repo_path.starts_with(allowed.as_str()));
                if !allowed {
                    return Ok(PolicyDecision::Deny {
                        reason: format!(
                            "GitAdapter: repo path '{repo_path}' not in allowed_repos list"
                        ),
                    });
                }
            }
        }

        // Validate branch name for checkout
        if command == "checkout" {
            let branch = op.args.get(1).map(|s| s.as_str()).unwrap_or("");
            if branch.is_empty() || !is_valid_branch_name(branch) {
                return Ok(PolicyDecision::Deny {
                    reason: format!(
                        "GitAdapter: invalid branch name '{branch}' for checkout"
                    ),
                });
            }
        }

        // Policy check
        let operation_key = format!("git-{command}");
        if let Some(rule) = config.find_rule(&operation_key) {
            if rule.approval_required || rule.always_ask {
                return Ok(PolicyDecision::RequiresApproval {
                    message: format!("git-{command}/{repo_path} requires approval per policy"),
                });
            }
        }

        Ok(PolicyDecision::Allow)
    }

    async fn execute(
        &self,
        _state: &AppState,
        identity: &ClientIdentity,
        op: &HostOp,
    ) -> Result<ExecutionResult, AppError> {
        let command = op.command().unwrap_or("status");
        let repo_path = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("git: resource (repo path) is required".into()))?;

        info!(
            caller = %identity.cn,
            command = %command,
            repo = %repo_path,
            "GitAdapter executing"
        );

        let output = run_git(command, repo_path, op).await?;
        Ok(ExecutionResult::ok(output))
    }
}

/// Run git command in the given repository directory.
/// No shell: each arg is passed separately to avoid injection.
async fn run_git(command: &str, repo_path: &str, op: &HostOp) -> Result<String, AppError> {
    let mut args: Vec<&str> = vec![command];

    // Append sub-args based on command
    let branch_str;
    match command {
        "checkout" => {
            if let Some(branch) = op.args.get(1) {
                branch_str = branch.clone();
                args.push(branch_str.as_str());
            }
        }
        "fetch" => {
            args.push("origin");
        }
        "log" => {
            args.push("--oneline");
            args.push("-10");
        }
        _ => {}
    }

    let output = Command::new(GIT_BIN)
        .current_dir(repo_path)
        .args(&args)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to spawn git: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(AppError::Internal(format!(
            "git {command} in {repo_path} failed: {stderr}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_branch_names() {
        assert!(is_valid_branch_name("main"));
        assert!(is_valid_branch_name("feature/my-feature"));
        assert!(is_valid_branch_name("release-1.2.3"));
        assert!(is_valid_branch_name("hotfix_123"));
        assert!(is_valid_branch_name("v1.0.0"));
    }

    #[test]
    fn test_invalid_branch_names() {
        // Path traversal
        assert!(!is_valid_branch_name("../../etc/passwd"));
        assert!(!is_valid_branch_name("../main"));
        // Shell injection
        assert!(!is_valid_branch_name("main; rm -rf /"));
        assert!(!is_valid_branch_name("$(whoami)"));
        assert!(!is_valid_branch_name("`id`"));
        // Empty
        assert!(!is_valid_branch_name(""));
        // Starts with hyphen (git flag injection)
        assert!(!is_valid_branch_name("-D"));
        assert!(!is_valid_branch_name("--force"));
        // Ends with slash
        assert!(!is_valid_branch_name("feature/"));
        // Contains ..
        assert!(!is_valid_branch_name("feat..test"));
    }

    #[test]
    fn test_repo_path_validation() {
        // Absolute paths that don't exist return false (no dir)
        assert!(!is_valid_repo_path("/nonexistent/path/repo"));
        // Relative path
        assert!(!is_valid_repo_path("relative/path"));
        // Path traversal
        assert!(!is_valid_repo_path("/srv/../etc"));
        // Empty
        assert!(!is_valid_repo_path(""));
    }

    #[test]
    fn test_repo_path_existing() {
        // /tmp always exists
        assert!(is_valid_repo_path("/tmp"));
        // /etc exists
        assert!(is_valid_repo_path("/etc"));
    }
}
