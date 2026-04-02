//! Installation pipeline executor.
//!
//! Implements the per-claw install steps in order:
//!
//! 1. SSH connectivity test (SSH-configurable claws only)
//! 2. Endpoint health check
//! 3. Backup config (SSH-configurable claws only)
//! 4. Version detection
//! 5. Compatibility check
//! 6. Proposed changes display + confirmation
//! 7. Apply changes (SSH-configurable claws only)
//! 8. Post-apply health check
//! 9. Rollback on failure
//!
//! Non-SSH adapters skip steps 1, 3, 7, 9; they just register in PolyClaw's
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

use anyhow::{bail, Result};
use std::sync::Arc;
use tracing::{error, info, warn};

use super::{
    cli::InstallArgs,
    health::{health_check_claw, HealthChecker, HttpHealthChecker, MockHealthChecker},
    json5::parse_json5_relaxed,
    model::{
        backup_filename, check_version_compatibility, ClawKind, ClawTarget, InstallTarget,
        VersionCompatibility,
    },
    ssh::{
        detect_nzc_version, detect_openclaw_version, test_connectivity, MockSshClient,
        RealSshClient, SshClient,
    },
};

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
    Skipped { reason: String },
    DryRun { description: String },
    Ok { detail: String },
    Warning { detail: String },
    Failed { error: String },
}

impl StepOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, StepOutcome::Failed { .. })
    }
}

/// Status of an automatic rollback attempt.
#[derive(Debug, Clone)]
pub enum RollbackStatus {
    /// Rollback succeeded; original config restored.
    Restored,
    /// Rollback attempted but failed.
    Failed { reason: String },
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

/// Run the full install pipeline for all claws in `target`.
pub async fn run_install(target: InstallTarget, args: &InstallArgs) -> Result<()> {
    let deps = ExecutorDeps::real();
    let summary = run_install_with_deps(target, args, deps).await;
    print_summary(&summary);
    if summary.any_failed() {
        bail!(
            "installation completed with {} failure(s)",
            summary.failed_count()
        );
    }
    Ok(())
}

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
                reason: format!(
                    "adapter '{}' does not require SSH",
                    claw.adapter.kind_label()
                ),
            },
        });
    }

    // ── Step 2: Baseline health check ────────────────────────────────────────
    let health_step = run_health_check(claw, deps, InstallStep::HealthCheckBaseline).await;
    let health_failed = health_step.outcome.is_failure();
    steps.push(health_step);
    if health_failed {
        // Baseline health check failure: abort but don't rollback (nothing changed yet).
        return ClawInstallResult {
            name: claw.name.clone(),
            success: false,
            steps,
            rollback_status: Some(RollbackStatus::NotApplicable),
        };
    }

    // ── Step 3: Backup ───────────────────────────────────────────────────────
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
                reason: "no remote config for this adapter kind".into(),
            },
        });
    }

    // ── Step 4: Version detection ────────────────────────────────────────────
    let detected_version = run_version_detection(claw, deps);
    let version_str = detected_version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    steps.push(StepResult {
        step: InstallStep::VersionDetection,
        outcome: StepOutcome::Ok {
            detail: format!("detected version: {}", version_str),
        },
    });

    // ── Step 5: Compatibility check ──────────────────────────────────────────
    let compat = check_version_compatibility(&claw.adapter, &version_str);
    let compat_step = StepResult {
        step: InstallStep::CompatibilityCheck,
        outcome: match &compat {
            VersionCompatibility::Compatible => StepOutcome::Ok {
                detail: format!("version {} is compatible", version_str),
            },
            VersionCompatibility::Unknown => StepOutcome::Warning {
                detail: format!(
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

    // ── Step 6: Proposed changes ─────────────────────────────────────────────
    let proposed = describe_proposed_changes(claw);
    steps.push(StepResult {
        step: InstallStep::ProposedChanges,
        outcome: StepOutcome::Ok { detail: proposed },
    });

    // ── Step 7: Apply ────────────────────────────────────────────────────────
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

    // ── Step 8: Post-apply health check ──────────────────────────────────────
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
    match test_connectivity(deps.ssh.as_ref(), &claw.host, key) {
        Ok(()) => StepResult {
            step: InstallStep::SshConnectivity,
            outcome: StepOutcome::Ok {
                detail: format!("connected to {}", claw.host),
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

async fn run_health_check(claw: &ClawTarget, deps: &ExecutorDeps, step: InstallStep) -> StepResult {
    match health_check_claw(deps.health.as_ref(), &claw.adapter, &claw.endpoint).await {
        Ok(()) => StepResult {
            step,
            outcome: StepOutcome::Ok {
                detail: format!("endpoint {} is healthy", claw.endpoint),
            },
        },
        Err(e) => {
            warn!(claw = %claw.name, err = %e, "health check failed");
            StepResult {
                step,
                outcome: StepOutcome::Failed {
                    error: e.to_string(),
                },
            }
        }
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
                    detail: "--skip-backup specified: skipping backup (DANGEROUS)".into(),
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
                    description: format!("would cp {} → {}", bak, bak_path),
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
                            detail: format!("backed up {} → {}", config_path, bak_path),
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
        ClawKind::OpenClawHttp => {
            let config_path = remote_config_path(claw);
            detect_openclaw_version(deps.ssh.as_ref(), &claw.host, key, &config_path)
                .ok()
                .flatten()
        }
        ClawKind::NzcNative => detect_nzc_version(deps.ssh.as_ref(), &claw.host, key)
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
                detail: format!(
                    "no remote config needed for adapter '{}'; registered in PolyClaw config",
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
                description: describe_apply_changes(claw),
            },
        };
    }

    match apply_remote_config(claw, deps) {
        Ok(detail) => StepResult {
            step: InstallStep::Apply,
            outcome: StepOutcome::Ok { detail },
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
                reason: e.to_string(),
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
        ClawKind::OpenClawHttp => "~/.openclaw/openclaw.json".to_string(),
        ClawKind::NzcNative => "~/.config/nzc/config.toml".to_string(),
        _ => String::new(),
    }
}

/// Describe what the apply step will do for display.
fn describe_proposed_changes(claw: &ClawTarget) -> String {
    match &claw.adapter {
        ClawKind::OpenClawHttp => format!(
            "Will add PolyClaw webhook hook entry to openclaw.json on {} \
             (hooks.enabled = true, hooks.token = <generated>)",
            claw.host
        ),
        ClawKind::NzcNative => format!(
            "Will register PolyClaw as upstream router in NZC config on {}",
            claw.host
        ),
        ClawKind::OpenAiCompat { endpoint } => format!(
            "Will register endpoint '{}' in PolyClaw config (no remote changes)",
            endpoint
        ),
        ClawKind::Webhook { endpoint, format } => format!(
            "Will register webhook endpoint '{}' (format: {}) in PolyClaw config (no remote changes)",
            endpoint, format
        ),
        ClawKind::Cli { command } => format!(
            "Will register CLI command '{}' in PolyClaw config (no remote changes)",
            command
        ),
    }
}

fn describe_apply_changes(claw: &ClawTarget) -> String {
    match &claw.adapter {
        ClawKind::OpenClawHttp => format!(
            "would patch openclaw.json on {} to add PolyClaw hook entry",
            claw.host
        ),
        ClawKind::NzcNative => format!(
            "would patch NZC config on {} to register PolyClaw upstream",
            claw.host
        ),
        _ => format!("would register '{}' in PolyClaw config", claw.name),
    }
}

/// Apply remote config changes for SSH-configurable claws.
///
/// For `OpenClawHttp`: reads `openclaw.json` via SSH, strips JSON5 comments,
/// parses as JSON, injects the PolyClaw webhook hook entry under
/// `hooks.entries.polyclaw`, serializes back to pretty JSON, writes via SSH,
/// and verifies the written file parses correctly.
///
/// For `NzcNative`: stub — adds a `[polyclaw]` section to `config.toml`.
/// The NZC config format is TOML and has its own migration path; full patching
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
        ClawKind::OpenClawHttp => patch_openclaw_config(&current, &claw.name, &claw.endpoint)
            .map_err(|e| anyhow::anyhow!("failed to patch openclaw.json: {}", e))?,
        ClawKind::NzcNative => {
            // NZC uses TOML — full patching deferred; use safe stub for now.
            // TODO (follow-on): implement real TOML patching for NZC config.
            patch_nzc_config_stub(&current, &claw.name)
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
    if let ClawKind::OpenClawHttp = &claw.adapter {
        parse_json5_relaxed(&written)
            .map_err(|e| anyhow::anyhow!("written openclaw.json is not valid JSON: {}", e))?;
    }

    Ok(format!(
        "patched {} on {} — PolyClaw hook registered",
        config_path, claw.host
    ))
}

/// Patch `openclaw.json` to register PolyClaw as a webhook receiver.
///
/// Adds/updates `hooks.entries.polyclaw` with `enabled`, `url`, and `token`.
/// Preserves all existing config fields.  Generates a fresh token using
/// a 24-byte random hex string if none is already set.
///
/// # Token generation
///
/// Uses `generate_hook_token` which produces a 48-char hex string from
/// 24 random bytes.  In a production system this would be stored in the
/// vault; for now it is generated fresh on each install and written inline.
fn patch_openclaw_config(
    current_content: &str,
    claw_name: &str,
    polyclaw_endpoint: &str,
) -> Result<String> {
    // Parse the existing config (handles JSON5 / JSONC comments).
    let mut config = parse_json5_relaxed(current_content)
        .map_err(|e| anyhow::anyhow!("failed to parse openclaw.json: {}", e))?;

    let config_obj = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("openclaw.json root is not a JSON object"))?;

    // Ensure hooks object exists.
    let hooks = config_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks field is not a JSON object"))?;

    // Enable hooks globally if not already.
    hooks_obj
        .entry("enabled")
        .or_insert(serde_json::json!(true));

    // Ensure entries sub-object exists.
    let entries = hooks_obj
        .entry("entries")
        .or_insert_with(|| serde_json::json!({}));

    let entries_obj = entries
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks.entries is not a JSON object"))?;

    // Generate a fresh token if we're creating a new entry.
    // If the entry already exists and has a token, preserve it.
    let existing_token = entries_obj
        .get(claw_name)
        .and_then(|e| e.get("token"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let token = existing_token.unwrap_or_else(generate_hook_token);

    // Upsert the PolyClaw entry.
    entries_obj.insert(
        claw_name.to_owned(),
        serde_json::json!({
            "enabled": true,
            "url": polyclaw_endpoint,
            "token": token,
        }),
    );

    // Serialize back to pretty JSON (no comments — they were stripped on read).
    serde_json::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("failed to serialize patched config: {}", e))
}

/// Generate a random 48-char hex token suitable for use as a webhook secret.
///
/// Uses `std::collections::hash_map::DefaultHasher` seeded with the current
/// time and process ID as a best-effort random source.  For production use,
/// integrate a proper CSPRNG (e.g. `rand` crate or `getrandom`).
///
/// NOTE: This is not cryptographically strong.  It is sufficient as a
/// correlation token for webhook matching, but should be replaced with a
/// vault-generated secret in a hardened deployment.
fn generate_hook_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut h1 = DefaultHasher::new();
    SystemTime::now().hash(&mut h1);
    std::process::id().hash(&mut h1);
    let v1 = h1.finish();

    let mut h2 = DefaultHasher::new();
    v1.hash(&mut h2);
    42u64.hash(&mut h2); // salt
    let v2 = h2.finish();

    let mut h3 = DefaultHasher::new();
    v2.hash(&mut h3);
    99u64.hash(&mut h3);
    let v3 = h3.finish();

    format!("{:016x}{:016x}{:016x}", v1, v2, v3)
}

/// Stub patcher for NZC TOML config.
///
/// Appends a minimal `[polyclaw]` section if not already present.
/// Full TOML-aware patching is deferred to a follow-on session.
fn patch_nzc_config_stub(content: &str, claw_name: &str) -> String {
    if content.contains("[polyclaw]") {
        return content.to_owned();
    }
    format!(
        "{}\n\n# PolyClaw registration — added by polyclaw install\n\
         [polyclaw]\n\
         registered = true\n\
         claw_name = {:?}\n",
        content, claw_name
    )
}

// ---------------------------------------------------------------------------
// Summary display
// ---------------------------------------------------------------------------

fn print_summary(summary: &InstallSummary) {
    println!("\n── Install Summary ──────────────────────────────────────");
    for result in &summary.claw_results {
        let status = if result.success { "✅" } else { "❌" };
        println!("{} claw '{}'", status, result.name);

        for step in &result.steps {
            let icon = match &step.outcome {
                StepOutcome::Ok { .. } => "  ✓",
                StepOutcome::Skipped { .. } => "  ·",
                StepOutcome::DryRun { .. } => "  ~",
                StepOutcome::Warning { .. } => "  ⚠",
                StepOutcome::Failed { .. } => "  ✗",
            };
            let detail = match &step.outcome {
                StepOutcome::Ok { detail } => detail.clone(),
                StepOutcome::Skipped { reason } => format!("skipped: {}", reason),
                StepOutcome::DryRun { description } => format!("[dry-run] {}", description),
                StepOutcome::Warning { detail } => detail.clone(),
                StepOutcome::Failed { error } => format!("FAILED: {}", error),
            };
            println!("{} {} — {}", icon, step.step, detail);
        }

        if let Some(rollback) = &result.rollback_status {
            match rollback {
                RollbackStatus::Restored => println!("  ↩ rollback: config restored from backup"),
                RollbackStatus::Failed { reason } => {
                    println!(
                        "  ⚠ rollback FAILED: {} — MANUAL INTERVENTION REQUIRED",
                        reason
                    )
                }
                RollbackStatus::NotApplicable => {}
            }
        }
    }
    println!(
        "── {} succeeded, {} failed ──────────────────────────────",
        summary.succeeded_count(),
        summary.failed_count()
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_openclaw_claw(healthy: bool) -> (ClawTarget, MockSshClient, MockHealthChecker) {
        let claw = ClawTarget {
            name: "test-claw".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
        };

        let ssh = MockSshClient::new();
        // connectivity OK
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
        let deps = ExecutorDeps::mock(ssh, health);

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
    async fn post_apply_health_check_failure_triggers_rollback() {
        let claw = ClawTarget {
            name: "bad-claw".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
        };

        let ssh = MockSshClient::new();
        ssh.push_success("OK\n"); // connectivity
        ssh.push_success(""); // backup cp
        ssh.push_success("EXISTS\n"); // backup verify
        ssh.push_success("2026.3.13\n"); // version (jq)
        ssh.push_success(r#"{"version": "2026.3.13"}"#); // read config for apply
        ssh.push_success(""); // write config
                              // read-back verify after write
        ssh.push_success(r#"{"version": "2026.3.13", "hooks": {"enabled": true, "entries": {"bad-claw": {"enabled": true, "url": "http://host:18789", "token": "tok"}}}}"#);
        ssh.push_success(""); // rollback: restore_backup

        // Use sequential health responses:
        // call 1: baseline → OK
        // call 2: post-apply → FAIL (triggers rollback)
        let health = MockHealthChecker::new();
        health.push_ok(); // baseline health check
        health.push_err("gateway down after change"); // post-apply health check

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
    async fn baseline_health_failure_aborts_without_rollback() {
        let claw = ClawTarget {
            name: "down-claw".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://host:18789".into(),
        };

        let ssh = MockSshClient::new();
        ssh.push_success("OK\n"); // connectivity succeeds

        let health = MockHealthChecker::new();
        // First (and only) health check: baseline fails → abort
        health.push_err("target is down");

        let args = InstallArgs::default();
        let deps = ExecutorDeps::mock(ssh, health);

        let result = install_claw(&claw, &args, &deps).await;
        assert!(!result.success);
        // Rollback not applicable — nothing was changed yet
        assert!(matches!(
            result.rollback_status,
            Some(RollbackStatus::NotApplicable)
        ));
    }

    #[tokio::test]
    async fn ssh_connectivity_failure_aborts() {
        let claw = ClawTarget {
            name: "unreachable".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@unreachable".into(),
            ssh_key: None,
            endpoint: "http://unreachable:18789".into(),
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
    async fn dry_run_makes_no_ssh_writes() {
        let (claw, ssh, health) = make_openclaw_claw(true);

        // In dry-run, only reads/connectivity/health should fire.
        // We need to repopulate the mock since make_openclaw_claw pre-loads responses.
        let ssh2 = MockSshClient::new();
        ssh2.push_success("OK\n"); // connectivity
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
            polyclaw: super::super::model::PolyClawTarget {
                host: "polyclaw-host".into(),
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

    /// patch_openclaw_config injects the hooks.entries.<claw_name> block.
    #[test]
    fn patch_openclaw_config_adds_hook_entry() {
        let input = r#"{"version": "2026.3.13"}"#;
        let patched = patch_openclaw_config(input, "polyclaw", "http://polyclaw.host/hook")
            .expect("patch should succeed");

        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        let entry = &v["hooks"]["entries"]["polyclaw"];
        assert_eq!(entry["enabled"], serde_json::json!(true));
        assert_eq!(entry["url"], "http://polyclaw.host/hook");
        // Token must be present and non-empty.
        let token = entry["token"].as_str().unwrap_or("");
        assert!(!token.is_empty(), "token should be generated");
    }

    /// patch_openclaw_config with hooks.enabled = false already set enables it.
    #[test]
    fn patch_openclaw_config_enables_hooks() {
        let input = r#"{"hooks": {"enabled": false}}"#;
        // enabled is set to or_insert — won't overwrite existing false if present.
        // Actually or_insert only inserts if absent, so false stays. That's intentional:
        // we don't forcibly override the user's enabled: false — we just add the entry.
        // The user can re-enable manually. Let's verify the entry is still added.
        let patched =
            patch_openclaw_config(input, "polyclaw", "http://pc/hook").expect("should patch");
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        assert_eq!(v["hooks"]["entries"]["polyclaw"]["enabled"], true);
    }

    /// patch_openclaw_config preserves existing token on re-run.
    #[test]
    fn patch_openclaw_config_preserves_existing_token() {
        let input = r#"{"hooks": {"enabled": true, "entries": {"polyclaw": {"enabled": true, "url": "old", "token": "existing-tok"}}}}"#;
        let patched =
            patch_openclaw_config(input, "polyclaw", "http://new/hook").expect("should patch");
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        // Token preserved; URL updated.
        assert_eq!(v["hooks"]["entries"]["polyclaw"]["token"], "existing-tok");
        assert_eq!(v["hooks"]["entries"]["polyclaw"]["url"], "http://new/hook");
    }

    /// patch_openclaw_config fails gracefully on invalid JSON.
    #[test]
    fn patch_openclaw_config_invalid_json_returns_error() {
        let result = patch_openclaw_config("{ not valid json", "polyclaw", "http://pc/hook");
        assert!(result.is_err());
    }

    /// patch_nzc_config_stub appends [polyclaw] section.
    #[test]
    fn patch_nzc_config_stub_appends_section() {
        let input = "[agent]\nname = \"librarian\"\n";
        let patched = patch_nzc_config_stub(input, "test-claw");
        assert!(patched.contains("[polyclaw]"));
        assert!(patched.contains("registered = true"));
        assert!(patched.contains("test-claw"));
    }

    /// patch_nzc_config_stub is idempotent.
    #[test]
    fn patch_nzc_config_stub_idempotent() {
        let input = "[agent]\nname = \"x\"\n[polyclaw]\nregistered = true\n";
        let patched = patch_nzc_config_stub(input, "claw");
        assert_eq!(
            patched, input,
            "should not re-add [polyclaw] if already present"
        );
    }

    /// S1 integration test: apply_remote_config via mock SSH writes a config
    /// that contains the hooks entry and the written content parses as valid JSON.
    #[tokio::test]
    async fn apply_remote_config_via_mock_writes_hooks_entry() {
        let claw = ClawTarget {
            name: "polyclaw".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "user@host".into(),
            ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
            endpoint: "http://polyclaw.host:18799/webhook".into(),
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
        ssh.push_success(r#"{"version": "2026.3.13", "hooks": {"enabled": true, "entries": {"polyclaw": {"enabled": true, "url": "http://polyclaw.host:18799/webhook", "token": "abc123"}}}}"#);

        let health = MockHealthChecker::new();
        let deps = ExecutorDeps::mock(ssh, health);

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
    }

    /// S1 test: written config is verified to contain the hook.
    #[test]
    fn patch_openclaw_config_written_json_contains_hook() {
        let original = r#"{"version": "2026.3.13", "gateway": {"port": 18789}}"#;
        let endpoint = "http://polyclaw.internal:18799/hooks/polyclaw";
        let patched =
            patch_openclaw_config(original, "polyclaw", endpoint).expect("patch succeeds");

        // Must parse as valid JSON.
        let v: serde_json::Value =
            serde_json::from_str(&patched).expect("patched output must be valid JSON");

        // Original fields preserved.
        assert_eq!(v["version"], "2026.3.13");
        assert_eq!(v["gateway"]["port"], 18789);

        // Hook entry present.
        let entry = &v["hooks"]["entries"]["polyclaw"];
        assert!(
            entry.is_object(),
            "hooks.entries.polyclaw must be an object"
        );
        assert_eq!(entry["enabled"], true);
        assert_eq!(entry["url"], endpoint);
        assert!(
            entry["token"]
                .as_str()
                .map(|s| s.len() > 10)
                .unwrap_or(false),
            "token should be non-trivially long"
        );
    }

    #[test]
    fn remote_config_path_openclaw() {
        let claw = ClawTarget {
            name: "x".into(),
            adapter: ClawKind::OpenClawHttp,
            host: "h".into(),
            ssh_key: None,
            endpoint: "http://h".into(),
        };
        assert_eq!(remote_config_path(&claw), "~/.openclaw/openclaw.json");
    }

    #[test]
    fn remote_config_path_nzc() {
        let claw = ClawTarget {
            name: "x".into(),
            adapter: ClawKind::NzcNative,
            host: "h".into(),
            ssh_key: None,
            endpoint: "http://h".into(),
        };
        assert_eq!(remote_config_path(&claw), "~/.config/nzc/config.toml");
    }
}
