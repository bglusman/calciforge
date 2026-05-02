//! Installation pipeline executor.
//!
//! Implements the per-claw install steps in order:
//!
//! 1. SSH connectivity test (SSH-configurable claws only)
//! 2. Remote config permission preflight (SSH-configurable claws only)
//! 3. Endpoint health check
//! 4. Backup config (SSH-configurable claws only)
//! 5. Version detection
//! 6. Compatibility check
//! 7. Proposed changes display + confirmation
//! 8. Apply changes (SSH-configurable claws only)
//! 9. Post-apply health check
//! 10. Rollback on failure
//!
//! Non-SSH adapters skip steps 1, 3, 7, 9; they just register in Calciforge's
//! config and pass the health check.
//!
//! # Dry-run
//!
//! When `args.dry_run` is true, every destructive action is logged but skipped.
//! Health checks still run (they're read-only).
//!
//! # Rollback
//!
//! If the post-apply health check fails, the executor automatically restores
//! the backup via `SshClient::restore_backup` and re-runs the health check.
//! The result (rollback ok / rollback also failed) is recorded in
//! [`ClawInstallResult`].

use anyhow::{bail, Context, Result};
use tracing::{error, info, warn};
use url::Url;

use crate::sync::Arc;

use super::{
    cli::InstallArgs,
    health::{health_check_claw, HealthChecker, HttpHealthChecker, MockHealthChecker},
    json5::parse_json5_relaxed,
    model::{
        backup_filename, check_version_compatibility, ClawKind, ClawTarget, InstallTarget,
        VersionCompatibility,
    },
    ssh::{
        detect_openclaw_version, detect_zeroclaw_version, ensure_openclaw_config_file,
        remote_path_shell, shell_quote, test_agent_target_connectivity, test_remote_config_access,
        MockSshClient, RealSshClient, SshClient,
    },
};

const DEFAULT_AGENT_NO_PROXY: &str = "localhost,127.0.0.1,::1";
const OPENCLAW_CHANNEL_PLUGIN_ID: &str = "calciforge-channel";
const OPENCLAW_POLICY_PLUGIN_ID: &str = "calciforge-policy";
const OPENCLAW_CHANNEL_PLUGIN_DIR: &str = "~/.openclaw/extensions/calciforge-channel";
const OPENCLAW_CHANNEL_PLUGIN_MANIFEST: &str =
    include_str!("../../../calciforge-openclaw-channel-plugin/openclaw.plugin.json");
const OPENCLAW_CHANNEL_PLUGIN_INDEX: &str =
    include_str!("../../../calciforge-openclaw-channel-plugin/index.js");
const OPENCLAW_CHANNEL_PLUGIN_PACKAGE: &str =
    include_str!("../../../calciforge-openclaw-channel-plugin/package.json");
const OPENCLAW_POLICY_PLUGIN_DIR: &str = "~/.openclaw/extensions/calciforge-policy";
const OPENCLAW_POLICY_PLUGIN_MANIFEST: &str =
    include_str!("../../../calciforge-policy-plugin/openclaw.plugin.json");
const OPENCLAW_POLICY_PLUGIN_INDEX: &str =
    include_str!("../../../calciforge-policy-plugin/dist/index.js");
const OPENCLAW_POLICY_PLUGIN_PACKAGE: &str =
    include_str!("../../../calciforge-policy-plugin/package.json");

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of installing a single claw.
#[derive(Debug, Clone)]
pub struct ClawInstallResult {
    pub name: String,
    pub success: bool,
    pub steps: Vec<StepResult>,
    pub rollback_status: Option<RollbackStatus>,
}

/// Outcome of a single installation step.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub step: InstallStep,
    pub outcome: StepOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStep {
    SshConnectivity,
    RemoteConfigAccess,
    HealthCheckBaseline,
    Backup,
    VersionDetection,
    CompatibilityCheck,
    ProposedChanges,
    Apply,
    HealthCheckPostApply,
}

impl std::fmt::Display for InstallStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallStep::SshConnectivity => write!(f, "SSH connectivity"),
            InstallStep::RemoteConfigAccess => write!(f, "remote config access"),
            InstallStep::HealthCheckBaseline => write!(f, "baseline health check"),
            InstallStep::Backup => write!(f, "config backup"),
            InstallStep::VersionDetection => write!(f, "version detection"),
            InstallStep::CompatibilityCheck => write!(f, "compatibility check"),
            InstallStep::ProposedChanges => write!(f, "proposed changes"),
            InstallStep::Apply => write!(f, "apply changes"),
            InstallStep::HealthCheckPostApply => write!(f, "post-apply health check"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StepOutcome {
    Skipped { _reason: String },
    DryRun { _description: String },
    Ok { _detail: String },
    Warning { _detail: String },
    Failed { error: String },
}

impl StepOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, StepOutcome::Failed { .. })
    }

    pub fn summary(&self) -> &str {
        match self {
            StepOutcome::Skipped { _reason } => _reason,
            StepOutcome::DryRun { _description } => _description,
            StepOutcome::Ok { _detail } => _detail,
            StepOutcome::Warning { _detail } => _detail,
            StepOutcome::Failed { error } => error,
        }
    }
}

/// Status of an automatic rollback attempt.
#[derive(Debug, Clone)]
pub enum RollbackStatus {
    /// Rollback succeeded; original config restored.
    Restored,
    /// Rollback attempted but failed.
    Failed { _reason: String },
    /// Rollback was not attempted (no backup taken, or not applicable).
    NotApplicable,
}

/// Summary of the full installation run.
#[derive(Debug)]
pub struct InstallSummary {
    pub claw_results: Vec<ClawInstallResult>,
}

impl InstallSummary {
    pub fn succeeded_count(&self) -> usize {
        self.claw_results.iter().filter(|r| r.success).count()
    }

    pub fn failed_count(&self) -> usize {
        self.claw_results.iter().filter(|r| !r.success).count()
    }

    pub fn any_failed(&self) -> bool {
        self.failed_count() > 0
    }
}

// ---------------------------------------------------------------------------
// Dependencies (injectable for tests)
// ---------------------------------------------------------------------------

pub struct ExecutorDeps {
    pub ssh: Arc<dyn SshClient>,
    pub health: Arc<dyn HealthChecker>,
}

impl ExecutorDeps {
    pub fn real() -> Self {
        Self {
            ssh: Arc::new(RealSshClient),
            health: Arc::new(HttpHealthChecker::new()),
        }
    }

    pub fn mock(ssh: MockSshClient, health: MockHealthChecker) -> Self {
        Self {
            ssh: Arc::new(ssh),
            health: Arc::new(health),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Run the install pipeline with injectable dependencies (used in tests).
pub async fn run_install_with_deps(
    target: InstallTarget,
    args: &InstallArgs,
    deps: ExecutorDeps,
) -> InstallSummary {
    if args.dry_run {
        info!("DRY RUN — no changes will be made");
    }

    let mut claw_results = Vec::new();

    for claw in &target.claws {
        info!(claw = %claw.name, "installing claw");
        let result = install_claw(claw, args, &deps).await;
        claw_results.push(result);
    }

    InstallSummary { claw_results }
}

// ---------------------------------------------------------------------------
// Per-claw installation pipeline
// ---------------------------------------------------------------------------

async fn install_claw(
    claw: &ClawTarget,
    args: &InstallArgs,
    deps: &ExecutorDeps,
) -> ClawInstallResult {
    let mut steps: Vec<StepResult> = Vec::new();
    let mut backup_path: Option<String> = None;
    #[allow(unused_assignments)]
    let mut rollback_status: Option<RollbackStatus> = None;

    // ── Step 1: SSH connectivity ─────────────────────────────────────────────
    if claw.needs_ssh_config() {
        let step = run_ssh_connectivity(claw, deps);
        let failed = step.outcome.is_failure();
        steps.push(step);
        if failed {
            return ClawInstallResult {
                name: claw.name.clone(),
                success: false,
                steps,
                rollback_status: Some(RollbackStatus::NotApplicable),
            };
        }
    } else {
        steps.push(StepResult {
            step: InstallStep::SshConnectivity,
            outcome: StepOutcome::Skipped {
                _reason: format!(
                    "adapter '{}' does not require SSH",
                    claw.adapter.kind_label()
                ),
            },
        });
    }

    // ── Step 2: Remote config permission preflight ───────────────────────────
    if claw.needs_ssh_config() {
        let step = run_remote_config_access(claw, deps);
        let failed = step.outcome.is_failure();
        steps.push(step);
        if failed {
            return ClawInstallResult {
                name: claw.name.clone(),
                success: false,
                steps,
                rollback_status: Some(RollbackStatus::NotApplicable),
            };
        }
    } else {
        steps.push(StepResult {
            step: InstallStep::RemoteConfigAccess,
            outcome: StepOutcome::Skipped {
                _reason: "no remote config for this adapter kind".into(),
            },
        });
    }

    // ── Step 3: Baseline health check ────────────────────────────────────────
    let health_step = run_health_check(claw, deps, InstallStep::HealthCheckBaseline).await;
    let health_failed = health_step.outcome.is_failure();
    if health_failed && matches!(claw.adapter, ClawKind::OpenClawChannel) {
        steps.push(StepResult {
            step: InstallStep::HealthCheckBaseline,
            outcome: StepOutcome::Warning {
                _detail: format!(
                    "{}; proceeding because installer-managed OpenClaw may not be running yet",
                    health_step.outcome.summary()
                ),
            },
        });
    } else {
        steps.push(health_step);
    }
    if health_failed && !matches!(claw.adapter, ClawKind::OpenClawChannel) {
        // Baseline health check failure: abort but don't rollback (nothing changed yet).
        return ClawInstallResult {
            name: claw.name.clone(),
            success: false,
            steps,
            rollback_status: Some(RollbackStatus::NotApplicable),
        };
    }

    // ── Step 4: Backup ───────────────────────────────────────────────────────
    if claw.needs_ssh_config() {
        let (backup_step, bak_path) = run_backup(claw, args, deps);
        let failed = backup_step.outcome.is_failure();
        backup_path = bak_path;
        steps.push(backup_step);
        if failed && !args.skip_backup {
            return ClawInstallResult {
                name: claw.name.clone(),
                success: false,
                steps,
                rollback_status: Some(RollbackStatus::NotApplicable),
            };
        }
    } else {
        steps.push(StepResult {
            step: InstallStep::Backup,
            outcome: StepOutcome::Skipped {
                _reason: "no remote config for this adapter kind".into(),
            },
        });
    }

    // ── Step 5: Version detection ────────────────────────────────────────────
    let detected_version = run_version_detection(claw, deps);
    let version_str = detected_version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    steps.push(StepResult {
        step: InstallStep::VersionDetection,
        outcome: StepOutcome::Ok {
            _detail: format!("detected version: {}", version_str),
        },
    });

    // ── Step 6: Compatibility check ──────────────────────────────────────────
    let compat = check_version_compatibility(&claw.adapter, &version_str);
    let compat_step = StepResult {
        step: InstallStep::CompatibilityCheck,
        outcome: match &compat {
            VersionCompatibility::Compatible => StepOutcome::Ok {
                _detail: format!("version {} is compatible", version_str),
            },
            VersionCompatibility::Unknown => StepOutcome::Warning {
                _detail: format!(
                    "version '{}' is not in the known-compatible list; proceeding with caution",
                    version_str
                ),
            },
            VersionCompatibility::Incompatible { reason } => StepOutcome::Failed {
                error: format!("version '{}' is incompatible: {}", version_str, reason),
            },
        },
    };
    let compat_failed = compat_step.outcome.is_failure();
    steps.push(compat_step);
    if compat_failed {
        return ClawInstallResult {
            name: claw.name.clone(),
            success: false,
            steps,
            rollback_status: Some(RollbackStatus::NotApplicable),
        };
    }

    // ── Step 7: Proposed changes ─────────────────────────────────────────────
    let proposed = describe_proposed_changes(claw);
    steps.push(StepResult {
        step: InstallStep::ProposedChanges,
        outcome: StepOutcome::Ok { _detail: proposed },
    });

    // ── Step 8: Apply ────────────────────────────────────────────────────────
    let apply_step = run_apply(claw, args, deps, backup_path.as_deref());
    let apply_failed = apply_step.outcome.is_failure();
    steps.push(apply_step);

    if apply_failed {
        // Attempt rollback if we have a backup.
        rollback_status = Some(attempt_rollback(claw, deps, backup_path.as_deref()));
        return ClawInstallResult {
            name: claw.name.clone(),
            success: false,
            steps,
            rollback_status,
        };
    }

    // ── Step 9: Post-apply health check ──────────────────────────────────────
    let post_health = run_health_check(claw, deps, InstallStep::HealthCheckPostApply).await;
    let post_failed = post_health.outcome.is_failure();
    steps.push(post_health);

    if post_failed {
        error!(claw = %claw.name, "post-apply health check failed — rolling back");
        rollback_status = Some(attempt_rollback(claw, deps, backup_path.as_deref()));
        return ClawInstallResult {
            name: claw.name.clone(),
            success: false,
            steps,
            rollback_status,
        };
    }

    ClawInstallResult {
        name: claw.name.clone(),
        success: true,
        steps,
        rollback_status: Some(RollbackStatus::NotApplicable),
    }
}

// ---------------------------------------------------------------------------
// Step implementations
// ---------------------------------------------------------------------------

fn run_ssh_connectivity(claw: &ClawTarget, deps: &ExecutorDeps) -> StepResult {
    let key = claw.ssh_key.as_deref();
    match test_agent_target_connectivity(deps.ssh.as_ref(), &claw.host, key) {
        Ok(()) => StepResult {
            step: InstallStep::SshConnectivity,
            outcome: StepOutcome::Ok {
                _detail: format!(
                    "connected to {} and confirmed non-Proxmox target",
                    claw.host
                ),
            },
        },
        Err(e) => {
            error!(claw = %claw.name, host = %claw.host, err = %e, "SSH connectivity failed");
            StepResult {
                step: InstallStep::SshConnectivity,
                outcome: StepOutcome::Failed {
                    error: e.to_string(),
                },
            }
        }
    }
}

fn run_remote_config_access(claw: &ClawTarget, deps: &ExecutorDeps) -> StepResult {
    let key = claw.ssh_key.as_deref();
    let config_path = remote_config_path(claw);
    if matches!(claw.adapter, ClawKind::OpenClawChannel) {
        if let Err(e) =
            ensure_openclaw_config_file(deps.ssh.as_ref(), &claw.host, key, &config_path)
        {
            return StepResult {
                step: InstallStep::RemoteConfigAccess,
                outcome: StepOutcome::Failed {
                    error: e.to_string(),
                },
            };
        }
    }
    match test_remote_config_access(deps.ssh.as_ref(), &claw.host, key, &config_path) {
        Ok(()) => StepResult {
            step: InstallStep::RemoteConfigAccess,
            outcome: StepOutcome::Ok {
                _detail: format!(
                    "remote config {} is readable and its directory is writable",
                    config_path
                ),
            },
        },
        Err(e) => {
            error!(claw = %claw.name, host = %claw.host, err = %e, "remote config access failed");
            StepResult {
                step: InstallStep::RemoteConfigAccess,
                outcome: StepOutcome::Failed {
                    error: e.to_string(),
                },
            }
        }
    }
}

async fn run_health_check(claw: &ClawTarget, deps: &ExecutorDeps, step: InstallStep) -> StepResult {
    let attempts = if step == InstallStep::HealthCheckPostApply {
        6
    } else {
        1
    };
    let mut last_err = None;

    for attempt in 1..=attempts {
        match health_check_claw(deps.health.as_ref(), &claw.adapter, &claw.endpoint).await {
            Ok(()) => {
                return StepResult {
                    step,
                    outcome: StepOutcome::Ok {
                        _detail: format!("endpoint {} is healthy", claw.endpoint),
                    },
                };
            }
            Err(e) => {
                warn!(claw = %claw.name, attempt, attempts, err = %e, "health check failed");
                last_err = Some(e);
                if attempt < attempts {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    StepResult {
        step,
        outcome: StepOutcome::Failed {
            error: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "health check failed".to_string()),
        },
    }
}

fn run_backup(
    claw: &ClawTarget,
    args: &InstallArgs,
    deps: &ExecutorDeps,
) -> (StepResult, Option<String>) {
    if args.skip_backup {
        return (
            StepResult {
                step: InstallStep::Backup,
                outcome: StepOutcome::Warning {
                    _detail: "--skip-backup specified: skipping backup (DANGEROUS)".into(),
                },
            },
            None,
        );
    }

    if args.dry_run {
        let bak = remote_config_path(claw);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let bak_path = backup_filename(&bak, ts);
        return (
            StepResult {
                step: InstallStep::Backup,
                outcome: StepOutcome::DryRun {
                    _description: format!("would cp {} → {}", bak, bak_path),
                },
            },
            Some(bak_path),
        );
    }

    let config_path = remote_config_path(claw);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let bak_path = backup_filename(&config_path, ts);
    let key = claw.ssh_key.as_deref();

    match deps
        .ssh
        .backup_file(&claw.host, key, &config_path, &bak_path)
    {
        Ok(()) => {
            // Verify the backup actually landed.
            match deps.ssh.verify_file_exists(&claw.host, key, &bak_path) {
                Ok(true) => (
                    StepResult {
                        step: InstallStep::Backup,
                        outcome: StepOutcome::Ok {
                            _detail: format!("backed up {} → {}", config_path, bak_path),
                        },
                    },
                    Some(bak_path),
                ),
                Ok(false) => (
                    StepResult {
                        step: InstallStep::Backup,
                        outcome: StepOutcome::Failed {
                            error: format!(
                                "backup command succeeded but {} not found on remote",
                                bak_path
                            ),
                        },
                    },
                    None,
                ),
                Err(e) => (
                    StepResult {
                        step: InstallStep::Backup,
                        outcome: StepOutcome::Failed {
                            error: format!("backup verification failed: {}", e),
                        },
                    },
                    None,
                ),
            }
        }
        Err(e) => {
            error!(claw = %claw.name, err = %e, "backup failed");
            (
                StepResult {
                    step: InstallStep::Backup,
                    outcome: StepOutcome::Failed {
                        error: e.to_string(),
                    },
                },
                None,
            )
        }
    }
}

fn run_version_detection(claw: &ClawTarget, deps: &ExecutorDeps) -> Option<String> {
    if !claw.needs_ssh_config() {
        return None;
    }
    let key = claw.ssh_key.as_deref();
    match &claw.adapter {
        ClawKind::OpenClawChannel => {
            let config_path = remote_config_path(claw);
            detect_openclaw_version(deps.ssh.as_ref(), &claw.host, key, &config_path)
                .ok()
                .flatten()
        }
        ClawKind::ZeroClawNative => detect_zeroclaw_version(deps.ssh.as_ref(), &claw.host, key)
            .ok()
            .flatten(),
        _ => None,
    }
}

fn run_apply(
    claw: &ClawTarget,
    args: &InstallArgs,
    deps: &ExecutorDeps,
    backup_path: Option<&str>,
) -> StepResult {
    // Non-SSH adapters: nothing to apply remotely.
    if !claw.needs_ssh_config() {
        return StepResult {
            step: InstallStep::Apply,
            outcome: StepOutcome::Ok {
                _detail: format!(
                    "no remote config needed for adapter '{}'; registered in Calciforge config",
                    claw.adapter.kind_label()
                ),
            },
        };
    }

    // Safety: backup must exist before we apply (unless --skip-backup was used).
    if backup_path.is_none() && !args.skip_backup {
        return StepResult {
            step: InstallStep::Apply,
            outcome: StepOutcome::Failed {
                error: "refusing to apply: no verified backup exists (use --skip-backup to override, but this is dangerous)".into(),
            },
        };
    }

    if args.dry_run {
        return StepResult {
            step: InstallStep::Apply,
            outcome: StepOutcome::DryRun {
                _description: describe_apply_changes(claw),
            },
        };
    }

    match apply_remote_config(claw, deps) {
        Ok(detail) => StepResult {
            step: InstallStep::Apply,
            outcome: StepOutcome::Ok { _detail: detail },
        },
        Err(e) => {
            error!(claw = %claw.name, err = %e, "apply failed");
            StepResult {
                step: InstallStep::Apply,
                outcome: StepOutcome::Failed {
                    error: e.to_string(),
                },
            }
        }
    }
}

fn attempt_rollback(
    claw: &ClawTarget,
    deps: &ExecutorDeps,
    backup_path: Option<&str>,
) -> RollbackStatus {
    let backup_path = match backup_path {
        Some(p) => p,
        None => {
            warn!(claw = %claw.name, "rollback requested but no backup path available");
            return RollbackStatus::NotApplicable;
        }
    };

    if !claw.needs_ssh_config() {
        return RollbackStatus::NotApplicable;
    }

    let config_path = remote_config_path(claw);
    let key = claw.ssh_key.as_deref();

    info!(claw = %claw.name, backup = %backup_path, "rolling back to backup");

    match deps
        .ssh
        .restore_backup(&claw.host, key, backup_path, &config_path)
    {
        Ok(()) => {
            info!(claw = %claw.name, "rollback succeeded");
            RollbackStatus::Restored
        }
        Err(e) => {
            error!(claw = %claw.name, err = %e, "rollback failed");
            RollbackStatus::Failed {
                _reason: e.to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config change logic (stubbed — expand per adapter in production)
// ---------------------------------------------------------------------------

/// The remote path to the config file for a claw, based on adapter kind.
fn remote_config_path(claw: &ClawTarget) -> String {
    match &claw.adapter {
        ClawKind::OpenClawChannel => "~/.openclaw/openclaw.json".to_string(),
        ClawKind::ZeroClawNative => "~/.config/zeroclaw/config.toml".to_string(),
        _ => String::new(),
    }
}

/// Describe what the apply step will do for display.
fn describe_proposed_changes(claw: &ClawTarget) -> String {
    match &claw.adapter {
        ClawKind::OpenClawChannel => format!(
            "Will update Calciforge OpenClaw integration config on {} \
             (installs calciforge-channel plugin, restarts openclaw-gateway, {}{})",
            claw.host,
            if claw.policy_endpoint.is_some() {
                "calciforge-policy enabled"
            } else {
                "no policy plugin change"
            },
            if claw.proxy_endpoint.is_some() {
                ", OpenClaw service proxy env configured"
            } else {
                ""
            }
        ),
        ClawKind::ZeroClawNative => format!(
            "Will register Calciforge as upstream router in ZeroClaw config on {}",
            claw.host
        ),
        ClawKind::OpenAiCompat { endpoint } => format!(
            "Will register endpoint '{}' in Calciforge config (no remote changes)",
            endpoint
        ),
        ClawKind::Webhook { endpoint, format } => format!(
            "Will register webhook endpoint '{}' (format: {}) in Calciforge config (no remote changes)",
            endpoint, format
        ),
        ClawKind::Cli { command } => format!(
            "Will register CLI command '{}' in Calciforge config (no remote changes)",
            command
        ),
    }
}

fn describe_apply_changes(claw: &ClawTarget) -> String {
    match &claw.adapter {
        ClawKind::OpenClawChannel => format!(
            "would patch openclaw.json on {} for Calciforge OpenClaw integration, install calciforge-channel plugin, and restart openclaw-gateway{}{}",
            claw.host,
            if claw.policy_endpoint.is_some() {
                " with policy plugin entry"
            } else {
                ""
            },
            if claw.proxy_endpoint.is_some() {
                " and configure OpenClaw service proxy env"
            } else {
                ""
            }
        ),
        ClawKind::ZeroClawNative => format!(
            "would patch ZeroClaw config on {} to register Calciforge upstream",
            claw.host
        ),
        _ => format!("would register '{}' in Calciforge config", claw.name),
    }
}

/// Apply remote config changes for SSH-configurable claws.
///
/// For `OpenClawChannel`: reads `openclaw.json` via SSH, strips JSON5 comments,
/// parses as JSON, removes obsolete Calciforge hook config that current
/// OpenClaw rejects, migrates the policy plugin entry, serializes back to
/// pretty JSON, writes via SSH, and verifies the written file parses correctly.
///
/// For `ZeroClawNative`: stub — adds a `[calciforge]` section to `config.toml`.
/// The ZeroClaw config format is TOML and has its own migration path; full patching
/// is deferred to a follow-on session.
fn apply_remote_config(claw: &ClawTarget, deps: &ExecutorDeps) -> Result<String> {
    let config_path = remote_config_path(claw);
    let key = claw.ssh_key.as_deref();

    // Read current config.
    let current = deps
        .ssh
        .read_file(&claw.host, key, &config_path)
        .map_err(|e| anyhow::anyhow!("failed to read remote config: {}", e))?;

    let patched = match &claw.adapter {
        ClawKind::OpenClawChannel => patch_openclaw_config(
            &current,
            &claw.name,
            claw.auth_token.as_deref(),
            claw.reply_webhook.as_deref(),
            claw.reply_auth_token.as_deref(),
            claw.policy_endpoint.as_deref(),
            claw.proxy_endpoint.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!("failed to patch openclaw.json: {}", e))?,
        ClawKind::ZeroClawNative => {
            // ZeroClaw uses TOML — full patching deferred; use safe stub for now.
            // TODO (follow-on): implement real TOML patching for ZeroClaw config.
            patch_zeroclaw_config_stub(&current, &claw.name)
        }
        _ => {
            // Non-SSH adapters should never reach apply_remote_config.
            return Err(anyhow::anyhow!(
                "apply_remote_config called for non-SSH adapter '{}'",
                claw.adapter.kind_label()
            ));
        }
    };

    deps.ssh
        .write_file(&claw.host, key, &config_path, &patched)
        .map_err(|e| anyhow::anyhow!("failed to write patched config: {}", e))?;

    // Verify the written file parses correctly (read-back check).
    let written = deps
        .ssh
        .read_file(&claw.host, key, &config_path)
        .map_err(|e| anyhow::anyhow!("failed to read back patched config: {}", e))?;

    // For OpenClaw: parse the written JSON to confirm it's valid.
    if let ClawKind::OpenClawChannel = &claw.adapter {
        parse_json5_relaxed(&written)
            .map_err(|e| anyhow::anyhow!("written openclaw.json is not valid JSON: {}", e))?;
    }

    let applied_detail = match claw.adapter {
        ClawKind::OpenClawChannel => "Calciforge OpenClaw channel config updated",
        ClawKind::ZeroClawNative => "Calciforge ZeroClaw upstream config updated",
        _ => "Calciforge remote agent config updated",
    };
    let mut details = vec![format!(
        "patched {} on {} — {}",
        config_path, claw.host, applied_detail
    )];

    if matches!(claw.adapter, ClawKind::OpenClawChannel) {
        install_remote_openclaw_channel_plugin(claw, deps)?;
        details.push("installed Calciforge OpenClaw channel plugin".to_string());
        if claw.policy_endpoint.is_some() {
            install_remote_openclaw_policy_plugin(claw, deps)?;
            details.push("installed Calciforge OpenClaw policy plugin".to_string());
        }

        let restarted_by_proxy =
            if let Some(detail) = configure_remote_openclaw_proxy_env(claw, deps)? {
                details.push(detail);
                true
            } else {
                false
            };
        if !restarted_by_proxy {
            details.push(restart_remote_openclaw_service(claw, deps)?);
        }
    }

    Ok(details.join("; "))
}

fn install_remote_openclaw_channel_plugin(claw: &ClawTarget, deps: &ExecutorDeps) -> Result<()> {
    let key = claw.ssh_key.as_deref();
    let mkdir = format!(
        "mkdir -p {}",
        remote_path_shell(OPENCLAW_CHANNEL_PLUGIN_DIR)
    );
    let mkdir_out = deps.ssh.run(&claw.host, key, &mkdir)?;
    if !mkdir_out.success {
        bail!(
            "failed to create OpenClaw channel plugin directory on {}: {}",
            claw.host,
            mkdir_out.stderr.trim()
        );
    }

    let manifest_path = format!("{OPENCLAW_CHANNEL_PLUGIN_DIR}/openclaw.plugin.json");
    let index_path = format!("{OPENCLAW_CHANNEL_PLUGIN_DIR}/index.js");
    let package_path = format!("{OPENCLAW_CHANNEL_PLUGIN_DIR}/package.json");
    deps.ssh.write_file(
        &claw.host,
        key,
        &manifest_path,
        OPENCLAW_CHANNEL_PLUGIN_MANIFEST,
    )?;
    deps.ssh
        .write_file(&claw.host, key, &index_path, OPENCLAW_CHANNEL_PLUGIN_INDEX)?;
    deps.ssh.write_file(
        &claw.host,
        key,
        &package_path,
        OPENCLAW_CHANNEL_PLUGIN_PACKAGE,
    )?;
    Ok(())
}

fn install_remote_openclaw_policy_plugin(claw: &ClawTarget, deps: &ExecutorDeps) -> Result<()> {
    let key = claw.ssh_key.as_deref();
    let mkdir = format!(
        "mkdir -p {}/dist",
        remote_path_shell(OPENCLAW_POLICY_PLUGIN_DIR)
    );
    let mkdir_out = deps.ssh.run(&claw.host, key, &mkdir)?;
    if !mkdir_out.success {
        bail!(
            "failed to create OpenClaw policy plugin directory on {}: {}",
            claw.host,
            mkdir_out.stderr.trim()
        );
    }

    let manifest_path = format!("{OPENCLAW_POLICY_PLUGIN_DIR}/openclaw.plugin.json");
    let index_path = format!("{OPENCLAW_POLICY_PLUGIN_DIR}/dist/index.js");
    let package_path = format!("{OPENCLAW_POLICY_PLUGIN_DIR}/package.json");
    deps.ssh.write_file(
        &claw.host,
        key,
        &manifest_path,
        OPENCLAW_POLICY_PLUGIN_MANIFEST,
    )?;
    deps.ssh
        .write_file(&claw.host, key, &index_path, OPENCLAW_POLICY_PLUGIN_INDEX)?;
    deps.ssh.write_file(
        &claw.host,
        key,
        &package_path,
        OPENCLAW_POLICY_PLUGIN_PACKAGE,
    )?;
    Ok(())
}

fn configure_remote_openclaw_proxy_env(
    claw: &ClawTarget,
    deps: &ExecutorDeps,
) -> Result<Option<String>> {
    let Some(proxy_endpoint) = claw
        .proxy_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    if proxy_endpoint.contains('\n') || proxy_endpoint.contains('\r') {
        bail!("proxy_endpoint must be a single-line URL");
    }

    let no_proxy = merged_openclaw_no_proxy(claw)?;
    if no_proxy.contains('\n') || no_proxy.contains('\r') {
        bail!("no_proxy must be a single-line value");
    }

    let key = claw.ssh_key.as_deref();
    let health_url = format!("{}/health", proxy_endpoint.trim_end_matches('/'));
    let proxy_health = deps.ssh.run(
        &claw.host,
        key,
        &format!(
            "curl -fsS --max-time 3 {} >/dev/null",
            shell_quote(&health_url)
        ),
    )?;
    if !proxy_health.success {
        bail!(
            "security-proxy endpoint is not reachable from {} at {}: {}",
            claw.host,
            health_url,
            proxy_health.stderr.trim()
        );
    }

    let service_mode = detect_openclaw_service_mode(claw, deps)?;
    if service_mode == "launchd" {
        return configure_launchd_openclaw_proxy_env(claw, deps, proxy_endpoint, &no_proxy);
    }

    let (dropin_dir, dropin_path, reload_restart) = match service_mode {
        "user" => (
            "~/.config/systemd/user/openclaw-gateway.service.d",
            "~/.config/systemd/user/openclaw-gateway.service.d/10-calciforge-proxy.conf",
            "systemctl --user daemon-reload && systemctl --user restart openclaw-gateway.service",
        ),
        "system" => (
            "/etc/systemd/system/openclaw-gateway.service.d",
            "/etc/systemd/system/openclaw-gateway.service.d/10-calciforge-proxy.conf",
            "systemctl daemon-reload && systemctl restart openclaw-gateway.service",
        ),
        other => bail!(
            "unexpected OpenClaw service mode from {}: {}",
            claw.host,
            other
        ),
    };

    let content = format!(
        "# Managed by calciforge install. Do not put secrets in this file.\n\
         # HTTPS inspection requires Calciforge MITM mode plus target runtime CA trust.\n\
         [Service]\n{}{}{}{}{}{}{}{}\n",
        systemd_environment_line("HTTP_PROXY", proxy_endpoint)?,
        systemd_environment_line("HTTPS_PROXY", proxy_endpoint)?,
        systemd_environment_line("ALL_PROXY", proxy_endpoint)?,
        systemd_environment_line("OPENCLAW_PROXY_URL", proxy_endpoint)?,
        systemd_environment_line("NO_PROXY", &no_proxy)?,
        systemd_environment_line("NODE_USE_SYSTEM_CA", "1")?,
        systemd_environment_line("CI", "true")?,
        systemd_environment_line("PNPM_CONFIG_CONFIRM_MODULES_PURGE", "false")?,
    );

    let mkdir = format!("mkdir -p {}", remote_path_shell(dropin_dir));
    let mkdir_out = deps.ssh.run(&claw.host, key, &mkdir)?;
    if !mkdir_out.success {
        bail!(
            "failed to create OpenClaw systemd drop-in directory on {}: {}",
            claw.host,
            mkdir_out.stderr.trim()
        );
    }

    deps.ssh
        .write_file(&claw.host, key, dropin_path, &content)
        .map_err(|e| anyhow::anyhow!("failed to write OpenClaw proxy drop-in: {}", e))?;

    let restart = deps.ssh.run(&claw.host, key, reload_restart)?;
    if !restart.success {
        bail!(
            "failed to reload/restart OpenClaw after proxy env install on {}: {}",
            claw.host,
            restart.stderr.trim()
        );
    }

    Ok(Some(format!(
        "configured OpenClaw {} service proxy env and noninteractive pnpm env via systemd drop-in",
        service_mode
    )))
}

fn configure_launchd_openclaw_proxy_env(
    claw: &ClawTarget,
    deps: &ExecutorDeps,
    proxy_endpoint: &str,
    no_proxy: &str,
) -> Result<Option<String>> {
    let key = claw.ssh_key.as_deref();
    let wrapper_path = "~/.config/calciforge/openclaw-gateway-wrapper.sh";
    let wrapper_dir = "~/.config/calciforge";
    let mkdir = deps.ssh.run(
        &claw.host,
        key,
        &format!("mkdir -p {}", remote_path_shell(wrapper_dir)),
    )?;
    if !mkdir.success {
        bail!(
            "failed to create OpenClaw launchd wrapper directory on {}: {}",
            claw.host,
            mkdir.stderr.trim()
        );
    }

    let wrapper = format!(
        "#!/bin/sh\n\
         # Managed by calciforge install. Do not put secrets in this file.\n\
         export HTTP_PROXY={}\n\
         export HTTPS_PROXY={}\n\
         export ALL_PROXY={}\n\
         export OPENCLAW_PROXY_URL={}\n\
         export NO_PROXY={}\n\
         export NODE_USE_SYSTEM_CA=1\n\
         export NODE_EXTRA_CA_CERTS=\"${{CALCIFORGE_MITM_CA_CERT:-$HOME/.config/calciforge/secrets/mitm-ca.pem}}\"\n\
         export CI=true\n\
         export PNPM_CONFIG_CONFIRM_MODULES_PURGE=false\n\
         exec openclaw \"$@\"\n",
        shell_export_value(proxy_endpoint),
        shell_export_value(proxy_endpoint),
        shell_export_value(proxy_endpoint),
        shell_export_value(proxy_endpoint),
        shell_export_value(no_proxy)
    );
    deps.ssh
        .write_file(&claw.host, key, wrapper_path, &wrapper)
        .map_err(|e| anyhow::anyhow!("failed to write OpenClaw launchd wrapper: {}", e))?;
    let chmod = deps.ssh.run(
        &claw.host,
        key,
        &format!("chmod 700 {}", remote_path_shell(wrapper_path)),
    )?;
    if !chmod.success {
        bail!(
            "failed to mark OpenClaw launchd wrapper executable on {}: {}",
            claw.host,
            chmod.stderr.trim()
        );
    }

    install_or_restart_launchd_openclaw_service(claw, deps, Some(wrapper_path))?;
    Ok(Some(
        "configured OpenClaw launchd service proxy env via managed wrapper".to_string(),
    ))
}

fn shell_export_value(value: &str) -> String {
    shell_quote(value)
}

fn merged_openclaw_no_proxy(claw: &ClawTarget) -> Result<String> {
    let base = claw
        .no_proxy
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_AGENT_NO_PROXY);
    if base.contains('\n') || base.contains('\r') {
        bail!("no_proxy must be a single-line value");
    }

    let mut entries = Vec::new();
    for part in base.split(',') {
        push_no_proxy_entry(&mut entries, part.trim());
    }

    if let Some(host) = ssh_no_proxy_host(&claw.host) {
        push_no_proxy_entry(&mut entries, &host);
    }
    push_url_host(&mut entries, &claw.endpoint, "endpoint")?;
    if let Some(reply_webhook) = claw.reply_webhook.as_deref() {
        push_url_host(&mut entries, reply_webhook, "reply_webhook")?;
    }
    if let Some(policy_endpoint) = claw.policy_endpoint.as_deref() {
        push_url_host(&mut entries, policy_endpoint, "policy_endpoint")?;
    }
    if let Some(proxy_endpoint) = claw.proxy_endpoint.as_deref() {
        push_url_host(&mut entries, proxy_endpoint, "proxy_endpoint")?;
    }

    Ok(entries.join(","))
}

fn push_url_host(entries: &mut Vec<String>, value: &str, field: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.contains('\n') || value.contains('\r') {
        bail!("{field} must be a single-line URL");
    }
    let url = Url::parse(value).with_context(|| format!("{field} must be a valid URL"))?;
    let Some(host) = url.host_str() else {
        bail!("{field} must include a host");
    };
    push_no_proxy_entry(entries, host);
    Ok(())
}

fn ssh_no_proxy_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.contains('\n') || host.contains('\r') {
        return None;
    }
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split_once(']').map(|(addr, _)| addr.to_string());
    }
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let host = host.trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn push_no_proxy_entry(entries: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if !entries.iter().any(|entry| entry == value) {
        entries.push(value.to_string());
    }
}

fn restart_remote_openclaw_service(claw: &ClawTarget, deps: &ExecutorDeps) -> Result<String> {
    let key = claw.ssh_key.as_deref();
    let service_mode = detect_openclaw_service_mode(claw, deps)?;
    if service_mode == "launchd" {
        install_or_restart_launchd_openclaw_service(claw, deps, None)?;
        return Ok("installed/restarted OpenClaw launchd service".to_string());
    }

    let restart = match service_mode {
        "user" => {
            "systemctl --user daemon-reload && systemctl --user restart openclaw-gateway.service"
        }
        "system" => "systemctl daemon-reload && systemctl restart openclaw-gateway.service",
        other => bail!(
            "unexpected OpenClaw service mode from {}: {}",
            claw.host,
            other
        ),
    };
    let out = deps.ssh.run(&claw.host, key, restart)?;
    if !out.success {
        bail!(
            "failed to restart OpenClaw gateway service on {}: {}",
            claw.host,
            out.stderr.trim()
        );
    }
    Ok(format!("restarted OpenClaw {} service", service_mode))
}

fn detect_openclaw_service_mode(claw: &ClawTarget, deps: &ExecutorDeps) -> Result<&'static str> {
    let key = claw.ssh_key.as_deref();
    let detect = deps.ssh.run(
        &claw.host,
        key,
        "if [ \"$(uname -s 2>/dev/null || true)\" = Darwin ]; then echo launchd; \
         elif systemctl is-active --quiet openclaw-gateway.service >/dev/null 2>&1; then echo system; \
         elif systemctl --user is-active --quiet openclaw-gateway.service >/dev/null 2>&1; then echo user; \
         elif systemctl cat openclaw-gateway.service >/dev/null 2>&1; then echo system; \
         elif systemctl --user cat openclaw-gateway.service >/dev/null 2>&1; then echo user; \
         else echo missing; exit 42; fi",
    )?;
    if !detect.success {
        bail!(
            "could not find openclaw-gateway.service on {}: {}",
            claw.host,
            detect.stderr.trim()
        );
    }
    match detect.stdout.trim() {
        "launchd" => Ok("launchd"),
        "user" => Ok("user"),
        "system" => Ok("system"),
        other => bail!(
            "unexpected OpenClaw service mode from {}: {}",
            claw.host,
            other
        ),
    }
}

fn install_or_restart_launchd_openclaw_service(
    claw: &ClawTarget,
    deps: &ExecutorDeps,
    wrapper_path: Option<&str>,
) -> Result<()> {
    let key = claw.ssh_key.as_deref();
    let port = endpoint_port(&claw.endpoint).unwrap_or(18789);
    let mut install = format!("openclaw gateway install --force --port {port}");
    if let Some(wrapper_path) = wrapper_path {
        install.push_str(" --wrapper ");
        install.push_str(&remote_path_shell(wrapper_path));
    }
    let command = format!(
        "set -eu; command -v openclaw >/dev/null 2>&1; {install}; openclaw gateway restart || openclaw gateway start"
    );
    let out = deps.ssh.run(&claw.host, key, &command)?;
    if !out.success {
        bail!(
            "failed to install/restart OpenClaw launchd service on {}: {}",
            claw.host,
            out.stderr.trim()
        );
    }
    Ok(())
}

fn endpoint_port(endpoint: &str) -> Option<u16> {
    Url::parse(endpoint).ok()?.port_or_known_default()
}

fn systemd_environment_line(key: &str, value: &str) -> Result<String> {
    if key.contains('=') || key.contains('\n') || key.contains('\r') {
        bail!("invalid environment key");
    }
    if value.contains('\n') || value.contains('\r') {
        bail!("invalid multiline environment value for {}", key);
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("Environment=\"{}={}\"\n", key, escaped))
}

/// Patch `openclaw.json` for Calciforge-managed OpenClaw integration.
///
/// Current OpenClaw channel integration uses `/calciforge/inbound` directly and
/// does not require a `hooks.entries.*` block. Modern OpenClaw rejects that old
/// shape, so this patcher only migrates legacy policy plugin names and enables
/// `plugins.entries.calciforge-policy` when a policy endpoint is provided.
///
/// Preserves all existing config fields.
fn patch_openclaw_config(
    current_content: &str,
    _claw_name: &str,
    auth_token: Option<&str>,
    reply_webhook: Option<&str>,
    reply_auth_token: Option<&str>,
    policy_endpoint: Option<&str>,
    proxy_endpoint: Option<&str>,
) -> Result<String> {
    // Parse the existing config (handles JSON5 / JSONC comments).
    let mut config = parse_json5_relaxed(current_content)
        .map_err(|e| anyhow::anyhow!("failed to parse openclaw.json: {}", e))?;

    let config_obj = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("openclaw.json root is not a JSON object"))?;

    remove_legacy_openclaw_hook_entries(config_obj)?;
    patch_openclaw_channel_plugin(config_obj, auth_token, reply_webhook, reply_auth_token)?;
    patch_openclaw_policy_plugin(config_obj, policy_endpoint)?;
    patch_openclaw_proxy_config(config_obj, proxy_endpoint)?;

    // Serialize back to pretty JSON (no comments — they were stripped on read).
    serde_json::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("failed to serialize patched config: {}", e))
}

fn patch_openclaw_proxy_config(
    config_obj: &mut serde_json::Map<String, serde_json::Value>,
    proxy_endpoint: Option<&str>,
) -> Result<()> {
    let Some(proxy_endpoint) = proxy_endpoint.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if proxy_endpoint.contains('\n') || proxy_endpoint.contains('\r') {
        bail!("proxy_endpoint must be a single-line URL");
    }
    let parsed = Url::parse(proxy_endpoint)
        .map_err(|e| anyhow::anyhow!("proxy_endpoint is invalid: {e}"))?;
    if parsed.scheme() != "http" {
        bail!("OpenClaw proxy_endpoint must use http://");
    }

    let proxy = config_obj
        .entry("proxy")
        .or_insert_with(|| serde_json::json!({}));
    let proxy_obj = proxy
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("proxy field is not a JSON object"))?;
    proxy_obj.insert("enabled".to_string(), serde_json::json!(true));
    proxy_obj.insert("proxyUrl".to_string(), serde_json::json!(proxy_endpoint));

    let browser = config_obj
        .entry("browser")
        .or_insert_with(|| serde_json::json!({}));
    let browser_obj = browser
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("browser field is not a JSON object"))?;
    let extra_args = browser_obj
        .entry("extraArgs")
        .or_insert_with(|| serde_json::json!([]));
    let extra_args = extra_args
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("browser.extraArgs is not an array"))?;

    let mut filtered_extra_args = Vec::with_capacity(extra_args.len() + 1);
    let mut skip_next_proxy_value = false;
    for arg in extra_args.drain(..) {
        if skip_next_proxy_value {
            skip_next_proxy_value = false;
            continue;
        }
        let Some(arg_str) = arg.as_str() else {
            filtered_extra_args.push(arg);
            continue;
        };
        if arg_str == "--no-proxy-server" {
            continue;
        }
        if arg_str == "--proxy-server" {
            skip_next_proxy_value = true;
            continue;
        }
        if arg_str.starts_with("--proxy-server=") {
            continue;
        }
        filtered_extra_args.push(arg);
    }
    *extra_args = filtered_extra_args;
    extra_args.push(serde_json::json!(format!(
        "--proxy-server={proxy_endpoint}"
    )));

    Ok(())
}

fn patch_openclaw_channel_plugin(
    config_obj: &mut serde_json::Map<String, serde_json::Value>,
    auth_token: Option<&str>,
    reply_webhook: Option<&str>,
    reply_auth_token: Option<&str>,
) -> Result<()> {
    let auth_token = required_single_line(auth_token, "auth_token")?;
    let reply_webhook = required_single_line(reply_webhook, "reply_webhook")?;
    let reply_auth_token = required_single_line(reply_auth_token, "reply_auth_token")?;

    let plugins = config_obj
        .entry("plugins")
        .or_insert_with(|| serde_json::json!({}));
    let plugins_obj = plugins
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("plugins field is not a JSON object"))?;
    plugins_obj.insert("enabled".to_string(), serde_json::json!(true));
    add_openclaw_plugin_allow_entry_if_present(plugins_obj, OPENCLAW_CHANNEL_PLUGIN_ID)?;

    let entries = plugins_obj
        .entry("entries")
        .or_insert_with(|| serde_json::json!({}));
    let entries_obj = entries
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("plugins.entries is not a JSON object"))?;

    for stale in ["polyclaw", "calciforge"] {
        entries_obj.remove(stale);
    }

    entries_obj.insert(
        OPENCLAW_CHANNEL_PLUGIN_ID.to_string(),
        serde_json::json!({
            "enabled": true,
            "config": {
                "authToken": auth_token,
                "replyWebhook": reply_webhook,
                "replyAuthToken": reply_auth_token,
            },
        }),
    );

    Ok(())
}

fn add_openclaw_plugin_allow_entry_if_present(
    plugins_obj: &mut serde_json::Map<String, serde_json::Value>,
    plugin_id: &str,
) -> Result<()> {
    let Some(allow) = plugins_obj.get_mut("allow") else {
        return Ok(());
    };
    let allow_arr = allow
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("plugins.allow is not an array"))?;
    if !allow_arr
        .iter()
        .any(|entry| entry.as_str() == Some(plugin_id))
    {
        allow_arr.push(serde_json::json!(plugin_id));
    }
    Ok(())
}

fn required_single_line<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    let value = value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("openclaw-channel install requires {field}"))?;
    if value.contains('\n') || value.contains('\r') {
        bail!("{field} must be a single-line value");
    }
    Ok(value)
}

fn remove_legacy_openclaw_hook_entries(
    config_obj: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let Some(hooks) = config_obj.get_mut("hooks") else {
        return Ok(());
    };
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks field is not a JSON object"))?;
    hooks_obj.remove("entries");
    if !hooks_obj.contains_key("token") {
        hooks_obj.remove("enabled");
    }
    if hooks_obj.is_empty() {
        config_obj.remove("hooks");
    }
    Ok(())
}

fn patch_openclaw_policy_plugin(
    config_obj: &mut serde_json::Map<String, serde_json::Value>,
    policy_endpoint: Option<&str>,
) -> Result<()> {
    let policy_endpoint = policy_endpoint.map(str::trim).filter(|s| !s.is_empty());
    if policy_endpoint.is_none() && !config_obj.contains_key("plugins") {
        return Ok(());
    }

    let plugins = config_obj
        .entry("plugins")
        .or_insert_with(|| serde_json::json!({}));
    let plugins_obj = plugins
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("plugins field is not a JSON object"))?;
    if policy_endpoint.is_some() {
        plugins_obj
            .entry("enabled")
            .or_insert(serde_json::json!(true));
        add_openclaw_plugin_allow_entry_if_present(plugins_obj, OPENCLAW_POLICY_PLUGIN_ID)?;
    }

    let entries = plugins_obj
        .entry("entries")
        .or_insert_with(|| serde_json::json!({}));
    let entries_obj = entries
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("plugins.entries is not a JSON object"))?;

    for stale in [
        "zeroclawed-policy",
        "polyclaw-policy",
        "polyclaw-plugin",
        "nonzeroclaw-policy",
    ] {
        entries_obj.remove(stale);
    }

    let Some(policy_endpoint) = policy_endpoint else {
        return Ok(());
    };

    entries_obj.insert(
        OPENCLAW_POLICY_PLUGIN_ID.to_string(),
        serde_json::json!({
            "enabled": true,
            "config": {
                "clashdEndpoint": policy_endpoint,
                "timeoutMs": 500,
                "fallbackOnError": "deny",
            },
        }),
    );

    Ok(())
}

/// Stub patcher for ZeroClaw TOML config.
///
/// Appends a minimal `[calciforge]` section if not already present.
/// Full TOML-aware patching is deferred to a follow-on session.
fn patch_zeroclaw_config_stub(content: &str, claw_name: &str) -> String {
    if content.contains("[calciforge]") {
        return content.to_owned();
    }
    format!(
        "{}\n\n# Calciforge registration — added by calciforge install\n\
         [calciforge]\n\
         registered = true\n\
         claw_name = {:?}\n",
        content, claw_name
    )
}

// ---------------------------------------------------------------------------
// Summary display
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::cli::parse_install_target;
    use super::*;
    use base64::Engine as _;
    use std::path::PathBuf;

    const TEST_AUTH_TOKEN: &str = "inbound-token";
    const TEST_REPLY_WEBHOOK: &str = "http://calciforge.host:18797/hooks/reply";
    const TEST_REPLY_AUTH_TOKEN: &str = "reply-token";

    fn push_openclaw_channel_plugin_install(ssh: &MockSshClient) {
        ssh.push_success(""); // mkdir extension dir
        ssh.push_success(""); // write openclaw.plugin.json
        ssh.push_success(""); // write index.js
        ssh.push_success(""); // write package.json
    }

    fn push_openclaw_policy_plugin_install(ssh: &MockSshClient) {
        ssh.push_success(""); // mkdir extension/dist dir
        ssh.push_success(""); // write openclaw.plugin.json
        ssh.push_success(""); // write dist/index.js
        ssh.push_success(""); // write package.json
    }

    fn push_openclaw_service_restart(ssh: &MockSshClient) {
        ssh.push_success("user\n"); // detect openclaw-gateway.service mode
        ssh.push_success(""); // restart service
    }

    fn push_openclaw_proxy_dropin(ssh: &MockSshClient) {
        ssh.push_success(""); // security-proxy /health from OpenClaw host
        ssh.push_success("user\n"); // detect openclaw-gateway.service mode
        ssh.push_success(""); // mkdir drop-in dir
        ssh.push_success(""); // write drop-in
        ssh.push_success(""); // reload/restart service
    }

    fn make_openclaw_claw(healthy: bool) -> (ClawTarget, MockSshClient, MockHealthChecker) {
        let claw = ClawTarget {
            name: "test-claw".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        // connectivity OK
        ssh.push_success("OK\n");
        // OpenClaw clean-install config bootstrap
        ssh.push_success("OK\n");
        // remote config permission preflight
        ssh.push_success("OK\n");
        // backup
        ssh.push_success(""); // cp
        ssh.push_success("EXISTS\n"); // verify
                                      // version detection (jq)
        ssh.push_success("2026.3.13\n");
        // apply: read config
        ssh.push_success(r#"{"version": "2026.3.13"}"#);
        // apply: write config
        ssh.push_success("");
        // apply: read-back verify (new in S1 implementation)
        ssh.push_success(r#"{"version": "2026.3.13", "hooks": {"enabled": true, "entries": {"test-claw": {"enabled": true, "url": "http://host:18789", "token": "tok"}}}}"#);
        push_openclaw_channel_plugin_install(&ssh);
        push_openclaw_service_restart(&ssh);

        // Use sequential health responses for both baseline and post-apply checks.
        let health = MockHealthChecker::new();
        if healthy {
            health.push_ok(); // baseline
            health.push_ok(); // post-apply
        } else {
            health.push_err("connection refused"); // baseline (triggers abort)
        }

        (claw, ssh, health)
    }

    fn make_openai_compat_claw() -> (ClawTarget, MockSshClient, MockHealthChecker) {
        let claw = ClawTarget {
            name: "openai-claw".into(),
            adapter: ClawKind::OpenAiCompat {
                endpoint: "http://llm.internal/v1".into(),
            },
            host: "llm.internal".into(),
            ssh_key: None,
            endpoint: "http://llm.internal/v1".into(),
            policy_endpoint: None,
            auth_token: None,
            reply_webhook: None,
            reply_auth_token: None,
            proxy_endpoint: None,
            no_proxy: None,
        };
        let ssh = MockSshClient::new();
        let health = MockHealthChecker::new();
        // OpenAI compat: baseline + post-apply (apply is a no-op but health still runs)
        health.push_ok(); // baseline
        health.push_ok(); // post-apply
        (claw, ssh, health)
    }

    #[tokio::test]
    async fn successful_openclaw_install() {
        let (claw, ssh, health) = make_openclaw_claw(true);
        let args = InstallArgs::default();
        let ssh = Arc::new(ssh);
        let deps = ExecutorDeps {
            ssh: ssh.clone(),
            health: Arc::new(health),
        };

        let result = install_claw(&claw, &args, &deps).await;
        assert!(
            result.success,
            "expected success, steps: {:?}",
            result.steps
        );
        // No rollback needed
        assert!(matches!(
            result.rollback_status,
            Some(RollbackStatus::NotApplicable)
        ));
    }

    #[tokio::test]
    async fn non_interactive_ephemeral_openclaw_install_runs_full_pipeline() {
        let args = InstallArgs {
            calciforge_host: Some("calciforge@ephemeral-runner.invalid".into()),
            calciforge_key: Some(PathBuf::from("/tmp/calciforge-ephemeral/id_ed25519")),
            claw_specs: vec![concat!(
                "name=matrix-e2e-openclaw,",
                "adapter=openclaw-channel,",
                "host=openclaw@ephemeral-runner.invalid,",
                "key=/tmp/calciforge-ephemeral/openclaw_id_ed25519,",
                "endpoint=http://127.0.0.1:18080,",
                "auth_token=inbound-token,",
                "reply_webhook=http://127.0.0.1:18797/hooks/reply,",
                "reply_auth_token=reply-token"
            )
            .to_string()],
            dry_run: false,
            skip_backup: false,
            _yes: true,
        };
        let target = parse_install_target(&args).expect("ephemeral install config should parse");
        assert_eq!(
            target.calciforge.host,
            "calciforge@ephemeral-runner.invalid"
        );
        assert_eq!(target.claws.len(), 1);
        assert_eq!(target.claws[0].name, "matrix-e2e-openclaw");
        assert!(matches!(target.claws[0].adapter, ClawKind::OpenClawChannel));

        let ssh = MockSshClient::new();
        ssh.push_success("OK\n");
        ssh.push_success("OK\n"); // OpenClaw clean-install config bootstrap
        ssh.push_success("OK\n");
        ssh.push_success("");
        ssh.push_success("EXISTS\n");
        ssh.push_success("2026.3.13\n");
        ssh.push_success(r#"{"version": "2026.3.13"}"#);
        ssh.push_success("");
        ssh.push_success(
            r#"{"version":"2026.3.13","hooks":{"enabled":true,"entries":{"matrix-e2e-openclaw":{"enabled":true,"url":"http://127.0.0.1:18080/hooks/calciforge","token":"tok"}}}}"#,
        );
        push_openclaw_channel_plugin_install(&ssh);
        push_openclaw_service_restart(&ssh);

        let health = MockHealthChecker::new();
        health.push_ok();
        health.push_ok();

        let summary = run_install_with_deps(target, &args, ExecutorDeps::mock(ssh, health)).await;
        assert_eq!(summary.succeeded_count(), 1, "{summary:?}");
        assert_eq!(summary.failed_count(), 0, "{summary:?}");
        assert!(!summary.any_failed(), "{summary:?}");

        let result = &summary.claw_results[0];
        assert_eq!(result.name, "matrix-e2e-openclaw");
        let executed_steps = result.steps.iter().map(|s| &s.step).collect::<Vec<_>>();
        let expected_steps = vec![
            &InstallStep::SshConnectivity,
            &InstallStep::RemoteConfigAccess,
            &InstallStep::HealthCheckBaseline,
            &InstallStep::Backup,
            &InstallStep::VersionDetection,
            &InstallStep::CompatibilityCheck,
            &InstallStep::ProposedChanges,
            &InstallStep::Apply,
            &InstallStep::HealthCheckPostApply,
        ];
        for expected in &expected_steps {
            assert!(
                executed_steps.contains(expected),
                "missing expected step {expected:?} in {executed_steps:?}"
            );
        }

        let index_of = |step: &InstallStep| {
            executed_steps
                .iter()
                .position(|executed| **executed == *step)
                .expect("expected step should be present")
        };
        assert!(
            index_of(&InstallStep::SshConnectivity) < index_of(&InstallStep::HealthCheckBaseline)
        );
        assert!(
            index_of(&InstallStep::SshConnectivity) < index_of(&InstallStep::RemoteConfigAccess)
        );
        assert!(
            index_of(&InstallStep::RemoteConfigAccess)
                < index_of(&InstallStep::HealthCheckBaseline)
        );
        assert!(index_of(&InstallStep::HealthCheckBaseline) < index_of(&InstallStep::Backup));
        assert!(
            index_of(&InstallStep::VersionDetection) < index_of(&InstallStep::CompatibilityCheck)
        );
        assert!(index_of(&InstallStep::ProposedChanges) < index_of(&InstallStep::Apply));
        assert!(index_of(&InstallStep::Apply) < index_of(&InstallStep::HealthCheckPostApply));
        assert!(matches!(
            result.rollback_status,
            Some(RollbackStatus::NotApplicable)
        ));
    }

    #[tokio::test]
    async fn post_apply_health_check_failure_triggers_rollback() {
        let claw = ClawTarget {
            name: "bad-claw".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        ssh.push_success("OK\n"); // connectivity
        ssh.push_success("OK\n"); // OpenClaw clean-install config bootstrap
        ssh.push_success("OK\n"); // remote config permission preflight
        ssh.push_success(""); // backup cp
        ssh.push_success("EXISTS\n"); // backup verify
        ssh.push_success("2026.3.13\n"); // version (jq)
        ssh.push_success(r#"{"version": "2026.3.13"}"#); // read config for apply
        ssh.push_success(""); // write config
                              // read-back verify after write
        ssh.push_success(r#"{"version": "2026.3.13", "hooks": {"enabled": true, "entries": {"bad-claw": {"enabled": true, "url": "http://host:18789", "token": "tok"}}}}"#);
        push_openclaw_channel_plugin_install(&ssh);
        push_openclaw_service_restart(&ssh);
        ssh.push_success(""); // rollback: restore_backup

        // Use sequential health responses:
        // call 1: baseline → OK
        // call 2: post-apply → FAIL (triggers rollback)
        let health = MockHealthChecker::new();
        health.push_ok(); // baseline health check
        for _ in 0..6 {
            health.push_err("gateway down after change"); // post-apply health check retries
        }

        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(!result.success, "should fail after health check");
        assert!(
            matches!(result.rollback_status, Some(RollbackStatus::Restored)),
            "rollback should have restored backup, got: {:?}",
            result.rollback_status
        );
    }

    #[tokio::test]
    async fn baseline_health_failure_warns_for_managed_openclaw() {
        let (claw, ssh, _health) = make_openclaw_claw(true);
        let health = MockHealthChecker::new();
        health.push_err("target is down");
        health.push_ok();

        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(
            result.success,
            "managed OpenClaw install should continue past pre-start health failure: {:?}",
            result.steps
        );
        let baseline = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::HealthCheckBaseline)
            .expect("baseline step");
        assert!(
            matches!(baseline.outcome, StepOutcome::Warning { .. }),
            "baseline health should be a warning for managed OpenClaw, got {:?}",
            baseline.outcome
        );
    }

    #[tokio::test]
    async fn ssh_connectivity_failure_aborts() {
        let claw = ClawTarget {
            name: "unreachable".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@unreachable".into(),
            ssh_key: None,
            endpoint: "http://unreachable:18789".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        ssh.push_failure("Connection refused");

        let health = MockHealthChecker::new();
        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(!result.success);
        let ssh_step = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::SshConnectivity)
            .unwrap();
        assert!(ssh_step.outcome.is_failure());
    }

    #[tokio::test]
    async fn remote_config_permission_failure_aborts_before_backup() {
        let claw = ClawTarget {
            name: "no-write".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        ssh.push_success("OK\n"); // connectivity
        ssh.push_success("OK\n"); // OpenClaw clean-install config bootstrap
        ssh.push_response(crate::install::ssh::SshOutput {
            stdout: String::new(),
            stderr: "remote config directory is not writable".to_string(),
            exit_code: 42,
            success: false,
        });

        let health = MockHealthChecker::new();
        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(!result.success);
        let access_step = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::RemoteConfigAccess)
            .unwrap();
        assert!(access_step.outcome.is_failure());
        assert!(!result.steps.iter().any(|s| s.step == InstallStep::Backup));
    }

    #[tokio::test]
    async fn dry_run_makes_no_ssh_writes() {
        let (claw, ssh, health) = make_openclaw_claw(true);

        // In dry-run, only reads/connectivity/health should fire.
        // We need to repopulate the mock since make_openclaw_claw pre-loads responses.
        let ssh2 = MockSshClient::new();
        ssh2.push_success("OK\n"); // connectivity
        ssh2.push_success("OK\n"); // OpenClaw clean-install config bootstrap
        ssh2.push_success("OK\n"); // remote config permission preflight
                                   // version detection (jq) — this is a read
        ssh2.push_success("2026.3.13\n");
        // No backup write, no apply write.
        drop(ssh); // don't use the original

        let args = InstallArgs {
            dry_run: true,
            ..Default::default()
        };
        let deps = ExecutorDeps::mock(ssh2, health);

        let result = install_claw(&claw, &args, &deps).await;
        // Dry run should "succeed" (no errors, just DryRun outcomes).
        let apply_step = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::Apply)
            .unwrap();
        assert!(
            matches!(apply_step.outcome, StepOutcome::DryRun { .. }),
            "apply in dry-run should be DryRun, got: {:?}",
            apply_step.outcome
        );
    }

    #[tokio::test]
    async fn openai_compat_claw_skips_ssh_steps() {
        let (claw, ssh, health) = make_openai_compat_claw();
        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(
            result.success,
            "openai-compat claw should succeed: {:?}",
            result.steps
        );

        // SSH connectivity step should be skipped
        let ssh_step = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::SshConnectivity)
            .unwrap();
        assert!(matches!(ssh_step.outcome, StepOutcome::Skipped { .. }));

        // Backup step should be skipped
        let bak_step = result
            .steps
            .iter()
            .find(|s| s.step == InstallStep::Backup)
            .unwrap();
        assert!(matches!(bak_step.outcome, StepOutcome::Skipped { .. }));
    }

    #[tokio::test]
    async fn full_install_summary_counts() {
        let (claw1, ssh1, health1) = make_openclaw_claw(true);
        let (claw2, ssh2, health2) = make_openai_compat_claw();

        let target = InstallTarget {
            calciforge: super::super::model::CalciforgeTarget {
                host: "calciforge-host".into(),
                ssh_key: None,
            },
            claws: vec![claw1, claw2],
        };

        let args = InstallArgs::default();

        // We need a single SshClient and HealthChecker for the whole run.
        // Use the first claw's ssh/health; for testing we'll run per-claw manually.
        let deps1 = ExecutorDeps::mock(ssh1, health1);
        let deps2 = ExecutorDeps::mock(ssh2, health2);

        // Run each claw individually to test the summary builder.
        let r1 = install_claw(&target.claws[0], &args, &deps1).await;
        let r2 = install_claw(&target.claws[1], &args, &deps2).await;

        let summary = InstallSummary {
            claw_results: vec![r1, r2],
        };
        assert_eq!(summary.succeeded_count(), 2);
        assert_eq!(summary.failed_count(), 0);
        assert!(!summary.any_failed());
    }

    // ── S1 tests: patch_openclaw_config and mock-SSH apply ───────────────────

    /// patch_openclaw_config does not inject legacy hooks.entries blocks.
    #[test]
    fn patch_openclaw_config_does_not_add_legacy_hook_entry() {
        let input = r#"{"version": "2026.3.13"}"#;
        let patched = patch_openclaw_config(
            input,
            "calciforge",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            None,
            None,
        )
        .expect("patch should succeed");

        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        assert!(
            v.get("hooks").is_none(),
            "modern OpenClaw rejects hooks.entries; installer must not add it"
        );
        assert_eq!(
            v["plugins"]["entries"]["calciforge-channel"]["config"]["authToken"],
            TEST_AUTH_TOKEN
        );
        assert_eq!(
            v["plugins"]["entries"]["calciforge-channel"]["config"]["replyWebhook"],
            TEST_REPLY_WEBHOOK
        );
        assert_eq!(
            v["plugins"]["entries"]["calciforge-channel"]["config"]["replyAuthToken"],
            TEST_REPLY_AUTH_TOKEN
        );
        assert!(
            v["plugins"].get("allow").is_none(),
            "installer must not create a restrictive allowlist when OpenClaw was auto-loading plugins"
        );
    }

    #[test]
    fn patch_openclaw_config_extends_existing_plugin_allowlist() {
        let input = r#"{"plugins": {"allow": ["kimi"]}}"#;
        let patched = patch_openclaw_config(
            input,
            "calciforge",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            Some("http://clashd.internal:9001/evaluate"),
            None,
        )
        .expect("patch should succeed");

        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        let allow = v["plugins"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|entry| entry.as_str() == Some("kimi")));
        assert!(allow
            .iter()
            .any(|entry| entry.as_str() == Some("calciforge-channel")));
        assert!(allow
            .iter()
            .any(|entry| entry.as_str() == Some("calciforge-policy")));
    }

    /// patch_openclaw_config preserves existing hooks fields without mutation.
    #[test]
    fn patch_openclaw_config_preserves_existing_hooks() {
        let input =
            r#"{"hooks": {"enabled": false, "entries": {"calciforge": {"enabled": true}}}}"#;
        let patched = patch_openclaw_config(
            input,
            "calciforge",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            None,
            None,
        )
        .expect("should patch");
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        assert!(
            v.get("hooks").is_none(),
            "hooks without a token are invalid in current OpenClaw and should be removed"
        );
    }

    /// patch_openclaw_config fails gracefully on invalid JSON.
    #[test]
    fn patch_openclaw_config_invalid_json_returns_error() {
        let result = patch_openclaw_config(
            "{ not valid json",
            "calciforge",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn patch_openclaw_config_migrates_policy_plugin_when_endpoint_set() {
        let input = r#"{
          "plugins": {
            "entries": {
              "zeroclawed-policy": {
                "enabled": true,
                "config": {"clashdEndpoint": "http://old.invalid/evaluate"}
              },
              "kimi": {"enabled": true}
            }
          }
        }"#;

        let patched = patch_openclaw_config(
            input,
            "custodian",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            Some("http://clashd.internal:9001/evaluate"),
            None,
        )
        .expect("patch succeeds");
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();

        let entries = &v["plugins"]["entries"];
        assert!(entries["zeroclawed-policy"].is_null());
        assert_eq!(entries["kimi"]["enabled"], true);
        assert_eq!(entries["calciforge-policy"]["enabled"], true);
        assert_eq!(
            entries["calciforge-policy"]["config"]["clashdEndpoint"],
            "http://clashd.internal:9001/evaluate"
        );
        assert_eq!(
            entries["calciforge-policy"]["config"]["fallbackOnError"],
            "deny"
        );
    }

    #[test]
    fn patch_openclaw_config_routes_browser_through_security_proxy() {
        let input = r#"{
          "proxy": {"enabled": false, "proxyUrl": "http://old-proxy.invalid:8888"},
          "browser": {
            "extraArgs": [
              "--window-size=1280,900",
              "--no-proxy-server",
              "--proxy-server=http://old-proxy.invalid:8888",
              "--proxy-server",
              "http://older-proxy.invalid:8888"
            ]
          }
        }"#;

        let patched = patch_openclaw_config(
            input,
            "custodian",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            None,
            Some("http://127.0.0.1:8888"),
        )
        .expect("patch succeeds");
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();

        assert_eq!(v["proxy"]["enabled"], true);
        assert_eq!(v["proxy"]["proxyUrl"], "http://127.0.0.1:8888");
        let extra_args = v["browser"]["extraArgs"].as_array().unwrap();
        assert!(extra_args
            .iter()
            .any(|arg| arg.as_str() == Some("--window-size=1280,900")));
        assert!(extra_args
            .iter()
            .any(|arg| arg.as_str() == Some("--proxy-server=http://127.0.0.1:8888")));
        assert!(!extra_args
            .iter()
            .any(|arg| arg.as_str() == Some("--no-proxy-server")));
        assert!(!extra_args
            .iter()
            .any(|arg| arg.as_str() == Some("http://older-proxy.invalid:8888")));
        assert_eq!(
            extra_args
                .iter()
                .filter(|arg| arg
                    .as_str()
                    .is_some_and(|arg| arg.starts_with("--proxy-server")))
                .count(),
            1
        );
    }

    /// patch_zeroclaw_config_stub appends [calciforge] section.
    #[test]
    fn patch_zeroclaw_config_stub_appends_section() {
        let input = "[agent]\nname = \"librarian\"\n";
        let patched = patch_zeroclaw_config_stub(input, "test-claw");
        assert!(patched.contains("[calciforge]"));
        assert!(patched.contains("registered = true"));
        assert!(patched.contains("test-claw"));
    }

    /// patch_zeroclaw_config_stub is idempotent.
    #[test]
    fn patch_zeroclaw_config_stub_idempotent() {
        let input = "[agent]\nname = \"x\"\n[calciforge]\nregistered = true\n";
        let patched = patch_zeroclaw_config_stub(input, "claw");
        assert_eq!(
            patched, input,
            "should not re-add [calciforge] if already present"
        );
    }

    /// apply_remote_config via mock SSH writes the OpenClaw plugin entry and
    /// the written content parses as valid JSON.
    #[tokio::test]
    async fn apply_remote_config_via_mock_writes_openclaw_channel_plugin_entry() {
        let claw = ClawTarget {
            name: "calciforge".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://calciforge.host:18799/webhook".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        // read_file: returns minimal openclaw.json
        ssh.push_success(r#"{"version": "2026.3.13"}"#);
        // write_file: success
        ssh.push_success("");
        // read_file again for verify (read-back)
        // We simulate the written content being stored by the mock.
        // MockSshClient's write_file records what was written; we need to
        // return the patched content on the second read.
        //
        // Since MockSshClient returns responses in order from a queue,
        // we push a valid patched JSON as the third response (read-back).
        ssh.push_success(r#"{"version": "2026.3.13", "plugins": {"enabled": true, "entries": {"calciforge-channel": {"enabled": true, "config": {"authToken": "inbound-token", "replyWebhook": "http://calciforge.host:18797/hooks/reply", "replyAuthToken": "reply-token"}}}}}"#);
        push_openclaw_channel_plugin_install(&ssh);
        push_openclaw_service_restart(&ssh);

        let health = MockHealthChecker::new();
        let ssh = Arc::new(ssh);
        let deps = ExecutorDeps {
            ssh: ssh.clone(),
            health: Arc::new(health),
        };

        let result = apply_remote_config(&claw, &deps);
        assert!(
            result.is_ok(),
            "apply_remote_config should succeed: {:?}",
            result
        );

        let detail = result.unwrap();
        assert!(
            detail.contains("patched"),
            "detail should mention patching: {}",
            detail
        );
        assert!(
            detail.contains("OpenClaw channel config updated"),
            "detail should name the OpenClaw channel config: {}",
            detail
        );
        assert!(
            detail.contains("installed Calciforge OpenClaw channel plugin"),
            "detail should mention plugin installation: {}",
            detail
        );

        let calls = ssh.recorded_calls();
        assert!(
            calls.iter().any(|c| c
                .command
                .contains(".openclaw/extensions/calciforge-channel/package.json")),
            "expected write to plugin package.json under OpenClaw extensions, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn apply_remote_config_configures_openclaw_proxy_dropin_when_requested() {
        let claw = ClawTarget {
            name: "calciforge".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://calciforge.host:18799/webhook".into(),
            policy_endpoint: Some("http://calciforge.host:9001/evaluate".into()),
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: Some("http://127.0.0.1:8888".into()),
            no_proxy: Some("localhost,127.0.0.1,::1,calciforge.host".into()),
        };

        let ssh = Arc::new(MockSshClient::new());
        ssh.push_success(r#"{"version": "2026.3.13"}"#);
        ssh.push_success("");
        ssh.push_success(
            r#"{"version":"2026.3.13","hooks":{"enabled":true,"entries":{"calciforge":{"enabled":true,"url":"http://calciforge.host:18799/webhook","token":"abc123"}}},"plugins":{"enabled":true,"entries":{"calciforge-policy":{"enabled":true,"config":{"clashdEndpoint":"http://calciforge.host:9001/evaluate"}}}}}"#,
        );
        push_openclaw_channel_plugin_install(&ssh);
        push_openclaw_policy_plugin_install(&ssh);
        push_openclaw_proxy_dropin(&ssh);

        let deps = ExecutorDeps {
            ssh: ssh.clone(),
            health: Arc::new(MockHealthChecker::new()),
        };
        let detail = apply_remote_config(&claw, &deps).expect("apply should succeed");
        assert!(detail.contains("proxy env"));
        assert!(
            detail.contains("installed Calciforge OpenClaw policy plugin"),
            "policy endpoint should install policy plugin files: {detail}"
        );

        let calls = ssh.recorded_calls();
        assert!(
            calls.iter().any(|c| c
                .command
                .contains(".openclaw/extensions/calciforge-policy/dist/index.js")),
            "expected write to policy plugin dist under OpenClaw extensions, got {calls:?}"
        );
        let dropin_write = calls
            .iter()
            .find(|c| {
                c.command
                    .contains("openclaw-gateway.service.d/10-calciforge-proxy.conf")
            })
            .expect("expected write to OpenClaw proxy drop-in");
        let b64 = dropin_write
            .command
            .strip_prefix("echo ")
            .and_then(|s| s.split_once(" | base64 -d > "))
            .map(|(encoded, _)| encoded.trim_matches('\''))
            .expect("drop-in write should be base64 encoded");
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .expect("drop-in content should decode"),
        )
        .expect("drop-in content should be utf-8");
        assert!(decoded.contains("Environment=\"HTTP_PROXY=http://127.0.0.1:8888\""));
        assert!(decoded.contains("Environment=\"HTTPS_PROXY=http://127.0.0.1:8888\""));
        assert!(decoded.contains("Environment=\"ALL_PROXY=http://127.0.0.1:8888\""));
        assert!(decoded.contains("Environment=\"OPENCLAW_PROXY_URL=http://127.0.0.1:8888\""));
        assert!(decoded.contains("Environment=\"CI=true\""));
        assert!(decoded.contains("Environment=\"PNPM_CONFIG_CONFIRM_MODULES_PURGE=false\""));
        assert!(
            calls.iter().any(|c| c
                .command
                .contains("systemctl --user restart openclaw-gateway.service")),
            "expected user service restart, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn apply_remote_config_installs_launchd_openclaw_service_on_macos() {
        let claw = ClawTarget {
            name: "calciforge".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "local".into(),
            ssh_key: None,
            endpoint: "http://127.0.0.1:18789".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://127.0.0.1:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = Arc::new(MockSshClient::new());
        ssh.push_success(r#"{"version": "2026.4.29"}"#);
        ssh.push_success("");
        ssh.push_success(r#"{"version":"2026.4.29","plugins":{"enabled":true,"entries":{"calciforge-channel":{"enabled":true,"config":{"authToken":"inbound-token","replyWebhook":"http://127.0.0.1:18797/hooks/reply","replyAuthToken":"reply-token"}}}}}"#);
        push_openclaw_channel_plugin_install(&ssh);
        ssh.push_success("launchd\n");
        ssh.push_success("");

        let deps = ExecutorDeps {
            ssh: ssh.clone(),
            health: Arc::new(MockHealthChecker::new()),
        };
        let detail = apply_remote_config(&claw, &deps).expect("apply should succeed");
        assert!(detail.contains("launchd service"));

        let calls = ssh.recorded_calls();
        assert!(
            calls.iter().any(|c| c
                .command
                .contains("openclaw gateway install --force --port 18789")),
            "expected OpenClaw launchd install command, got {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.command.contains("openclaw gateway restart")
                    || c.command.contains("openclaw gateway start")),
            "expected OpenClaw launchd restart/start command, got {calls:?}"
        );
    }

    #[test]
    fn openclaw_proxy_no_proxy_includes_managed_hosts() {
        let claw = ClawTarget {
            name: "calciforge".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "user@openclaw.host:22".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://openclaw.host:18799/webhook".into(),
            policy_endpoint: Some("http://clashd.host:9001/evaluate".into()),
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: Some("http://127.0.0.1:8888".into()),
            no_proxy: Some("localhost,127.0.0.1,::1,calciforge.host".into()),
        };

        let no_proxy = merged_openclaw_no_proxy(&claw).expect("valid no_proxy");
        assert_eq!(
            no_proxy,
            "localhost,127.0.0.1,::1,calciforge.host,openclaw.host,clashd.host"
        );
    }

    #[test]
    fn apply_remote_config_zeroclaw_detail_names_zeroclaw() {
        let claw = ClawTarget {
            name: "librarianzero".into(),
            adapter: ClawKind::ZeroClawNative,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://zeroclaw.host:18799/webhook".into(),
            policy_endpoint: None,
            auth_token: None,
            reply_webhook: None,
            reply_auth_token: None,
            proxy_endpoint: None,
            no_proxy: None,
        };

        let ssh = MockSshClient::new();
        ssh.push_success("[agent]\nname = \"librarian\"\n");
        ssh.push_success("");
        ssh.push_success(
            "[agent]\nname = \"librarian\"\n\n[calciforge]\nregistered = true\nagent = \"librarianzero\"\n",
        );
        let deps = ExecutorDeps::mock(ssh, MockHealthChecker::new());

        let detail = apply_remote_config(&claw, &deps).expect("apply should succeed");
        assert!(
            detail.contains("ZeroClaw upstream config updated"),
            "detail should name the ZeroClaw config: {detail}"
        );
        assert!(
            !detail.contains("OpenClaw channel"),
            "ZeroClaw detail must not mention OpenClaw channel config: {detail}"
        );
    }

    /// S1 test: written config remains valid JSON and preserves existing fields.
    #[test]
    fn patch_openclaw_config_written_json_preserves_existing_fields() {
        let original = r#"{"version": "2026.3.13", "gateway": {"port": 18789}}"#;
        let patched = patch_openclaw_config(
            original,
            "calciforge",
            Some(TEST_AUTH_TOKEN),
            Some(TEST_REPLY_WEBHOOK),
            Some(TEST_REPLY_AUTH_TOKEN),
            None,
            None,
        )
        .expect("patch succeeds");

        // Must parse as valid JSON.
        let v: serde_json::Value =
            serde_json::from_str(&patched).expect("patched output must be valid JSON");

        // Original fields preserved.
        assert_eq!(v["version"], "2026.3.13");
        assert_eq!(v["gateway"]["port"], 18789);
        assert!(v.get("hooks").is_none());
    }

    #[test]
    fn remote_config_path_openclaw() {
        let claw = ClawTarget {
            name: "x".into(),
            adapter: ClawKind::OpenClawChannel,
            host: "h".into(),
            ssh_key: None,
            endpoint: "http://h".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };
        assert_eq!(remote_config_path(&claw), "~/.openclaw/openclaw.json");
    }

    #[test]
    fn remote_config_path_zeroclaw() {
        let claw = ClawTarget {
            name: "x".into(),
            adapter: ClawKind::ZeroClawNative,
            host: "h".into(),
            ssh_key: None,
            endpoint: "http://h".into(),
            policy_endpoint: None,
            auth_token: Some("inbound-token".into()),
            reply_webhook: Some("http://calciforge.host:18797/hooks/reply".into()),
            reply_auth_token: Some("reply-token".into()),
            proxy_endpoint: None,
            no_proxy: None,
        };
        assert_eq!(remote_config_path(&claw), "~/.config/zeroclaw/config.toml");
    }
}
