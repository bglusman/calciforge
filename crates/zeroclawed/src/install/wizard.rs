//! Interactive TUI wizard for the ZeroClawed installer.
//!
//! Launched when `zeroclawed install` is run with no CLI flags. Guides the
//! operator through configuring ZeroClawed to route messages to one or more
//! downstream claws.
//!
//! # Wizard steps
//!
//! 1. **ZeroClawed host** — where is ZeroClawed running? (host, SSH key)
//! 2. **Add claws** — loop: name, adapter, host/key (for SSH kinds), endpoint
//! 3. **Test connections** — SSH connectivity + health check each target
//! 4. **Channel routing** — for each ZeroClawed-owned channel, which claw handles it?
//! 5. **Review** — show full summary of planned changes
//! 6. **Confirm** — explicit yes/no before any writes
//! 7. **Execute** — run per-claw install steps with progress
//! 8. **Summary** — what succeeded, what failed, rollback status
//!
//! # Non-TTY environments
//!
//! When `stdin` is not a TTY (e.g. piped input, CI), the wizard prints an
//! error and exits cleanly rather than panicking inside `dialoguer`.

use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Select};
use std::io::IsTerminal;
use std::path::PathBuf;

use super::{
    cli::InstallArgs,
    executor::{run_install_with_deps, ExecutorDeps, StepOutcome},
    health::{health_check_claw, HttpHealthChecker},
    model::{ClawKind, ClawTarget, InstallTarget, ZeroClawedTarget, WebhookFormat},
    ssh::{test_connectivity, RealSshClient},
};

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

const WIZARD_BANNER: &str = r#"
  ╔══════════════════════════════════════════════════════════╗
  ║         ZeroClawed Multi-Target Installer                  ║
  ║  Route messages from one ZeroClawed to many claws          ║
  ╚══════════════════════════════════════════════════════════╝
"#;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the interactive installation wizard.
///
/// Checks for TTY before starting; exits cleanly if stdin is not interactive.
pub async fn run_wizard() -> Result<()> {
    // Guard: dialoguer panics on non-TTY stdin.
    if !<std::io::Stdin as IsTerminal>::is_terminal(&std::io::stdin()) {
        bail!(
            "interactive wizard requires a TTY — use --zeroclawed-host / --claw flags for non-interactive mode"
        );
    }

    println!("{}", style(WIZARD_BANNER).cyan().bold());

    // ── Step 1: ZeroClawed host ─────────────────────────────────────────────
    println!("{}", style("Step 1/8 — ZeroClawed Host").bold().underlined());
    let zeroclawed = collect_zeroclawed_host()?;

    // ── Step 2: Add claws ─────────────────────────────────────────────────
    println!(
        "\n{}",
        style("Step 2/8 — Add Downstream Claws").bold().underlined()
    );
    let claws = collect_claws()?;

    if claws.is_empty() {
        bail!("no claws configured — nothing to install");
    }

    let target = InstallTarget { zeroclawed, claws };

    // ── Step 3: Test connections ──────────────────────────────────────────
    println!(
        "\n{}",
        style("Step 3/8 — Testing Connections").bold().underlined()
    );
    let connection_ok = test_connections(&target).await;
    if !connection_ok {
        let proceed = Confirm::new()
            .with_prompt("Some connection tests failed. Proceed anyway?")
            .default(false)
            .interact()
            .context("confirmation prompt failed")?;
        if !proceed {
            bail!("aborted by operator after connection test failures");
        }
    }

    // ── Step 4: Channel routing ───────────────────────────────────────────
    println!(
        "\n{}",
        style("Step 4/8 — Channel Routing").bold().underlined()
    );
    let routing = collect_channel_routing(&target)?;
    display_routing_summary(&routing);

    // ── Step 5: Review ────────────────────────────────────────────────────
    println!("\n{}", style("Step 5/8 — Review").bold().underlined());
    display_install_review(&target);

    // ── Step 6: Confirm ───────────────────────────────────────────────────
    println!("\n{}", style("Step 6/8 — Confirm").bold().underlined());
    let confirmed = Confirm::new()
        .with_prompt("Apply these changes? This will modify remote configs.")
        .default(false)
        .interact()
        .context("confirmation prompt failed")?;

    if !confirmed {
        println!("{}", style("Installation cancelled.").yellow());
        return Ok(());
    }

    // ── Step 7: Execute ───────────────────────────────────────────────────
    println!("\n{}", style("Step 7/8 — Executing").bold().underlined());
    let args = InstallArgs {
        zeroclawed_host: Some(target.zeroclawed.host.clone()),
        zeroclawed_key: target.zeroclawed.ssh_key.clone(),
        ..Default::default()
    };

    let deps = ExecutorDeps::real();
    let summary = run_install_with_deps(target, &args, deps).await;

    // ── Step 8: Summary ───────────────────────────────────────────────────
    println!("\n{}", style("Step 8/8 — Summary").bold().underlined());
    for result in &summary.claw_results {
        if result.success {
            println!(
                "  {} claw '{}' installed successfully",
                style("✅").green(),
                result.name
            );
        } else {
            println!("  {} claw '{}' FAILED", style("❌").red(), result.name);
            for step in &result.steps {
                if let StepOutcome::Failed { error } = &step.outcome {
                    println!("     ↳ {} failed: {}", step.step, error);
                }
            }
        }
    }

    if summary.any_failed() {
        bail!(
            "installation completed with {} failure(s)",
            summary.failed_count()
        );
    }

    println!(
        "\n{}",
        style("All claws installed successfully! ZeroClawed is ready.")
            .green()
            .bold()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Step 1: ZeroClawed host
// ---------------------------------------------------------------------------

fn collect_zeroclawed_host() -> Result<ZeroClawedTarget> {
    println!("  Where is ZeroClawed running?");
    println!("  (Enter 'local' if ZeroClawed runs on this machine)");

    let host: String = Input::new()
        .with_prompt("  ZeroClawed host (user@hostname or 'local')")
        .interact_text()
        .context("failed to read ZeroClawed host")?;

    let host = if host.trim() == "local" {
        "localhost".to_string()
    } else {
        host.trim().to_string()
    };

    let ssh_key = if host == "localhost" {
        None
    } else {
        let key_str: String = Input::new()
            .with_prompt("  SSH key path (leave blank for default ~/.ssh/id_rsa)")
            .allow_empty(true)
            .interact_text()
            .context("failed to read SSH key path")?;
        if key_str.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(key_str.trim()))
        }
    };

    Ok(ZeroClawedTarget { host, ssh_key })
}

// ---------------------------------------------------------------------------
// Step 2: Collect claws
// ---------------------------------------------------------------------------

fn collect_claws() -> Result<Vec<ClawTarget>> {
    let mut claws: Vec<ClawTarget> = Vec::new();

    println!("  Add the downstream claws ZeroClawed should route messages to.");
    println!(
        "  {} — knows the config format, can SSH in and apply changes safely.",
        style("NZC / OpenClaw").bold()
    );
    println!(
        "  {} — registered in ZeroClawed config; no remote config management.",
        style("Other adapters").bold()
    );

    loop {
        println!();

        let name: String = Input::new()
            .with_prompt("  Claw name (e.g. 'librarian', 'custodian')")
            .interact_text()
            .context("failed to read claw name")?;
        let name = name.trim().to_string();

        if name.is_empty() {
            println!("  {}", style("Name cannot be empty.").red());
            continue;
        }

        // Adapter selection.
        let adapter_options = &[
            "nzc          — NonZeroClaw native (SSH-configurable)",
            "openclaw     — OpenClaw HTTP gateway (SSH-configurable)",
            "openai-compat — OpenAI-compatible endpoint (endpoint-only)",
            "webhook      — Generic HTTP webhook (endpoint-only)",
            "cli          — Local binary (no network)",
        ];
        let adapter_idx = Select::new()
            .with_prompt("  Adapter kind")
            .items(adapter_options)
            .default(0)
            .interact()
            .context("failed to select adapter")?;

        let adapter_str = match adapter_idx {
            0 => "nzc",
            1 => "openclaw",
            2 => "openai-compat",
            3 => "webhook",
            4 => "cli",
            _ => unreachable!(),
        };

        let adapter = collect_adapter_config(adapter_str)?;
        let (host, ssh_key, endpoint) = collect_connectivity(adapter_str, &adapter)?;

        claws.push(ClawTarget {
            name,
            adapter,
            host,
            ssh_key,
            endpoint,
        });

        let add_another = Confirm::new()
            .with_prompt("  Add another claw?")
            .default(false)
            .interact()
            .context("failed to read confirmation")?;

        if !add_another {
            break;
        }
    }

    Ok(claws)
}

fn collect_adapter_config(adapter_str: &str) -> Result<ClawKind> {
    match adapter_str {
        "nzc" => Ok(ClawKind::NzcNative),
        "openclaw" => Ok(ClawKind::OpenClawHttp),
        "openai-compat" => {
            let endpoint: String = Input::new()
                .with_prompt("  OpenAI-compat endpoint (e.g. http://host/v1)")
                .interact_text()
                .context("failed to read endpoint")?;
            Ok(ClawKind::OpenAiCompat {
                endpoint: endpoint.trim().to_string(),
            })
        }
        "webhook" => {
            let endpoint: String = Input::new()
                .with_prompt("  Webhook endpoint URL")
                .interact_text()
                .context("failed to read endpoint")?;
            let format_opts = &[
                "json — POST {\"message\": \"...\"}",
                "text — POST raw text body",
            ];
            let fmt_idx = Select::new()
                .with_prompt("  Webhook format")
                .items(format_opts)
                .default(0)
                .interact()
                .context("failed to select format")?;
            let format = match fmt_idx {
                0 => WebhookFormat::Json,
                _ => WebhookFormat::Text,
            };
            Ok(ClawKind::Webhook {
                endpoint: endpoint.trim().to_string(),
                format,
            })
        }
        "cli" => {
            let command: String = Input::new()
                .with_prompt("  Binary path or command")
                .interact_text()
                .context("failed to read command")?;
            Ok(ClawKind::Cli {
                command: command.trim().to_string(),
            })
        }
        other => bail!("unknown adapter: {}", other),
    }
}

fn collect_connectivity(
    adapter_str: &str,
    adapter: &ClawKind,
) -> Result<(String, Option<PathBuf>, String)> {
    let needs_ssh = adapter.is_remotely_configurable();

    let host = if needs_ssh {
        let h: String = Input::new()
            .with_prompt("  Host (user@hostname)")
            .interact_text()
            .context("failed to read host")?;
        h.trim().to_string()
    } else {
        // For non-SSH adapters, host is optional / display only.
        String::new()
    };

    let ssh_key = if needs_ssh {
        let key_str: String = Input::new()
            .with_prompt("  SSH key path (leave blank for ~/.ssh/id_rsa)")
            .allow_empty(true)
            .interact_text()
            .context("failed to read SSH key")?;
        if key_str.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(key_str.trim()))
        }
    } else {
        None
    };

    // Endpoint — for SSH adapters (NZC/OpenClaw); non-SSH adapters set it
    // in collect_adapter_config, but we may need it here for display.
    let endpoint = match adapter {
        ClawKind::OpenAiCompat { endpoint } => endpoint.clone(),
        ClawKind::Webhook { endpoint, .. } => endpoint.clone(),
        ClawKind::Cli { .. } => String::new(),
        _ => {
            // NZC / OpenClaw: collect endpoint separately.
            let e: String = Input::new()
                .with_prompt(&format!("  {} endpoint URL", adapter_str))
                .interact_text()
                .context("failed to read endpoint")?;
            e.trim().to_string()
        }
    };

    Ok((host, ssh_key, endpoint))
}

// ---------------------------------------------------------------------------
// Step 3: Test connections
// ---------------------------------------------------------------------------

async fn test_connections(target: &InstallTarget) -> bool {
    let ssh = RealSshClient;
    let health = HttpHealthChecker::new();
    let mut all_ok = true;

    for claw in &target.claws {
        print!("  Testing '{}' ...", claw.name);

        if claw.needs_ssh_config() {
            match test_connectivity(&ssh, &claw.host, claw.ssh_key.as_deref()) {
                Ok(()) => print!(" {} SSH", style("✓").green()),
                Err(e) => {
                    print!(" {} SSH ({})", style("✗").red(), e);
                    all_ok = false;
                }
            }
        }

        if !claw.endpoint.is_empty() {
            match health_check_claw(&health, &claw.adapter, &claw.endpoint).await {
                Ok(()) => println!(" {} health", style("✓").green()),
                Err(e) => {
                    println!(" {} health ({})", style("✗").red(), e);
                    all_ok = false;
                }
            }
        } else {
            println!(" (no endpoint to check)");
        }
    }

    all_ok
}

// ---------------------------------------------------------------------------
// Step 4: Channel routing
// ---------------------------------------------------------------------------

/// A channel routing assignment: which claw handles this channel?
#[derive(Debug, Clone)]
pub struct ChannelRouting {
    pub channel_name: String,
    pub assigned_claw: String,
}

fn collect_channel_routing(target: &InstallTarget) -> Result<Vec<ChannelRouting>> {
    println!("  For each ZeroClawed-owned channel, choose which claw handles it.");
    println!("  (Skip this step if you'll configure routing manually in config.toml)");

    let has_channels = Confirm::new()
        .with_prompt("  Do you want to configure channel routing now?")
        .default(true)
        .interact()
        .context("failed to read confirmation")?;

    if !has_channels {
        return Ok(Vec::new());
    }

    let known_channels = &[
        "telegram",
        "signal",
        "whatsapp",
        "matrix",
        "discord",
        "(custom — enter manually)",
    ];

    let mut routings: Vec<ChannelRouting> = Vec::new();
    let claw_names: Vec<&str> = target.claws.iter().map(|c| c.name.as_str()).collect();

    loop {
        let ch_idx = Select::new()
            .with_prompt("  Channel to route")
            .items(known_channels)
            .default(0)
            .interact()
            .context("failed to select channel")?;

        let channel_name = if ch_idx == known_channels.len() - 1 {
            let custom: String = Input::new()
                .with_prompt("  Custom channel name")
                .interact_text()
                .context("failed to read channel name")?;
            custom.trim().to_string()
        } else {
            known_channels[ch_idx].to_string()
        };

        let claw_idx = Select::new()
            .with_prompt(format!("  Which claw handles '{}'?", channel_name))
            .items(&claw_names)
            .default(0)
            .interact()
            .context("failed to select claw")?;

        routings.push(ChannelRouting {
            channel_name: channel_name.clone(),
            assigned_claw: claw_names[claw_idx].to_string(),
        });

        let add_more = Confirm::new()
            .with_prompt("  Add another channel routing?")
            .default(false)
            .interact()
            .context("failed to read confirmation")?;

        if !add_more {
            break;
        }
    }

    Ok(routings)
}

fn display_routing_summary(routing: &[ChannelRouting]) {
    if routing.is_empty() {
        println!("  (no channel routing configured)");
        return;
    }
    println!("  Planned channel routing:");
    for r in routing {
        println!(
            "    {} → {}",
            style(&r.channel_name).bold(),
            r.assigned_claw
        );
    }
}

// ---------------------------------------------------------------------------
// Step 5: Review
// ---------------------------------------------------------------------------

fn display_install_review(target: &InstallTarget) {
    println!("  ZeroClawed host: {}", style(&target.zeroclawed.host).bold());
    println!("  Claws to configure ({}):", target.claws.len());
    for claw in &target.claws {
        println!(
            "    {} ({}) @ {}",
            style(&claw.name).bold(),
            claw.adapter.kind_label(),
            claw.endpoint
        );
        if claw.needs_ssh_config() {
            println!(
                "      SSH host: {}{}",
                claw.host,
                claw.ssh_key
                    .as_ref()
                    .map(|k| format!(" (key: {})", k.display()))
                    .unwrap_or_default()
            );
            println!("      Remote config: will be backed up before any changes");
        } else {
            println!("      No remote config changes (endpoint-only registration)");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (wizard helpers that don't require TTY)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_routing_struct_fields() {
        let r = ChannelRouting {
            channel_name: "telegram".into(),
            assigned_claw: "librarian".into(),
        };
        assert_eq!(r.channel_name, "telegram");
        assert_eq!(r.assigned_claw, "librarian");
    }

    #[test]
    fn display_routing_summary_empty() {
        // Should not panic on empty routing list.
        display_routing_summary(&[]);
    }

    #[test]
    fn display_routing_summary_nonempty() {
        let routing = vec![
            ChannelRouting {
                channel_name: "signal".into(),
                assigned_claw: "librarian".into(),
            },
            ChannelRouting {
                channel_name: "telegram".into(),
                assigned_claw: "custodian".into(),
            },
        ];
        // Should not panic.
        display_routing_summary(&routing);
    }

    #[test]
    fn display_install_review_no_panic() {
        let target = InstallTarget {
            zeroclawed: ZeroClawedTarget {
                host: "admin@10.0.0.1".into(),
                ssh_key: None,
            },
            claws: vec![
                ClawTarget {
                    name: "lib".into(),
                    adapter: ClawKind::NzcNative,
                    host: "user@10.0.0.20".into(),
                    ssh_key: Some(PathBuf::from("/keys/id_ed25519")),
                    endpoint: "http://10.0.0.20:18799".into(),
                },
                ClawTarget {
                    name: "openai".into(),
                    adapter: ClawKind::OpenAiCompat {
                        endpoint: "http://llm/v1".into(),
                    },
                    host: String::new(),
                    ssh_key: None,
                    endpoint: "http://llm/v1".into(),
                },
            ],
        };
        // Must not panic.
        display_install_review(&target);
    }

    /// Verify that the wizard entry point correctly detects non-TTY and bails.
    #[tokio::test]
    async fn wizard_errors_on_non_tty() {
        use std::io::IsTerminal as _;
        // In test environments stdin is typically not a TTY.
        if !std::io::stdin().is_terminal() {
            let result = run_wizard().await;
            assert!(result.is_err());
            let msg = result.err().unwrap().to_string();
            assert!(
                msg.contains("TTY") || msg.contains("interactive"),
                "should mention TTY/interactive: {}",
                msg
            );
        }
        // If somehow running in a TTY context, skip.
    }
}
