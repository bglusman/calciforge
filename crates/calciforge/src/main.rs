//! Calciforge — Rust agent gateway
//!
//! Entry point: loads config, starts enabled channels, routes messages to
//! configured downstream agents, and optionally serves the model gateway.

#![recursion_limit = "512"]

mod adapters;
mod agent_kinds;
mod artifacts;
mod auth;
mod channels;
mod commands;
mod config;
mod context;
mod doctor;
#[cfg(test)]
mod hooks;
#[allow(dead_code)] // installer has production CLI entrypoints plus wizard/test support.
mod install;
mod local_model;
mod messages;
#[cfg(feature = "persistent-context")]
mod persistent_context;
mod providers;
mod proxy;
mod router;
mod sync;
mod unified_context;
mod voice;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};

use crate::sync::Arc;

use adversary_detector::audit::AuditLogger;
use adversary_detector::middleware::ChannelScanner;
use adversary_detector::profiles::{SecurityConfig, SecurityProfile};
use adversary_detector::scanner::AdversaryScanner;

use crate::{
    commands::CommandHandler, providers::alloy::AlloyManager, router::Router,
    unified_context::UnifiedContextStore,
};

/// Calciforge — Rust agent gateway
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to config file (default: ~/.config/calciforge/config.toml, then legacy/system fallbacks)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Run only the proxy server, skip channels (for testing)
    #[arg(long, global = true)]
    proxy_only: bool,

    /// Validate config file and exit (don't start server)
    #[arg(long, global = true)]
    validate: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Diagnose common config, state, secret-file, and endpoint problems.
    Doctor {
        /// Skip TCP reachability checks.
        #[arg(long)]
        no_network: bool,
    },
    /// Configure Calciforge against existing agent installations over SSH.
    Install {
        /// Calciforge host metadata, e.g. user@host.
        #[arg(long)]
        calciforge_host: String,
        /// SSH private key for the Calciforge host.
        #[arg(long)]
        calciforge_key: Option<PathBuf>,
        /// Claw target spec. May be provided more than once.
        #[arg(long = "claw")]
        claw_specs: Vec<String>,
        /// Show planned changes without writing remote configs.
        #[arg(long)]
        dry_run: bool,
        /// Skip remote config backup before patching.
        #[arg(long)]
        skip_backup: bool,
        /// Skip interactive confirmations.
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI args
    let args = Args::parse();

    // Load config (from CLI arg or default path)
    let config_path = args
        .config
        .unwrap_or_else(|| config::config_path().expect("Failed to determine default config path"));

    if let Some(CliCommand::Doctor { no_network }) = args.command {
        let report = doctor::run(&config_path, no_network).await?;
        let has_errors = report.has_errors();
        report.print();
        std::process::exit(if has_errors { 1 } else { 0 });
    }

    if let Some(CliCommand::Install {
        calciforge_host,
        calciforge_key,
        claw_specs,
        dry_run,
        skip_backup,
        yes,
    }) = args.command
    {
        let install_args = install::cli::InstallArgs {
            calciforge_host: Some(calciforge_host),
            calciforge_key,
            claw_specs,
            dry_run,
            skip_backup,
            _yes: yes,
        };
        let target = install::cli::parse_install_target(&install_args)?;
        let summary = install::executor::run_install_with_deps(
            target,
            &install_args,
            install::executor::ExecutorDeps::real(),
        )
        .await;

        for result in &summary.claw_results {
            println!(
                "{} {}",
                if result.success { "ok" } else { "failed" },
                result.name
            );
            for step in &result.steps {
                println!("  {}: {}", step.step, step.outcome.summary());
            }
        }

        std::process::exit(if summary.any_failed() { 1 } else { 0 });
    }

    // If --validate flag is set, just validate and exit
    if args.validate {
        match config::validator::validate_config_file(&config_path) {
            Ok(validation) => {
                if validation.is_valid() {
                    println!("✅ Configuration is valid!");
                    if !validation.warnings.is_empty() {
                        println!("\n⚠️  Warnings:");
                        for warning in &validation.warnings {
                            println!("  - {}", warning);
                        }
                    }
                    std::process::exit(0);
                } else {
                    println!("❌ Configuration validation failed:");
                    for error in &validation.errors {
                        println!("  - {}", error);
                    }
                    if !validation.warnings.is_empty() {
                        println!("\n⚠️  Warnings:");
                        for warning in &validation.warnings {
                            println!("  - {}", warning);
                        }
                    }
                    std::process::exit(1);
                }
            }
            Err(e) => {
                println!("❌ Failed to validate config: {}", e);
                std::process::exit(1);
            }
        }
    }

    // Initialize tracing — respects RUST_LOG env var. Diagnostic commands above
    // print their own concise output and avoid startup log noise.
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("calciforge=info".parse()?))
        .init();

    info!("Calciforge starting");
    info!(path = %config_path.display(), "loading config");

    let config = config::load_config_from(&config_path).with_context(|| {
        format!(
            "Failed to load config from {}. Create it first (see README).",
            config_path.display()
        )
    })?;

    info!(
        version = config.calciforge.version,
        identities = config.identities.len(),
        agents = config.agents.len(),
        channels = config.channels.len(),
        buffer_size = config.context.buffer_size,
        inject_depth = config.context.inject_depth,
        "config loaded"
    );
    // Debug: log any agent aliases at startup
    for agent in &config.agents {
        if !agent.aliases.is_empty() {
            info!(agent = %agent.id, aliases = ?agent.aliases, "agent aliases registered");
        }
    }

    let unified_context_store = UnifiedContextStore::new(
        config.context.buffer_size,
        config.context.inject_depth,
        config.context.persistent.as_ref(),
    )
    .await?;

    // Persistent context is feature-gated; when enabled it must be plumbed through
    // all channel and command handler call sites (currently in-memory only).
    let context_store_arc = unified_context_store.into_in_memory()?;

    // Clone the inner ContextStore for channel functions
    let context_store = (*context_store_arc).clone();

    // Initialize adversary detector middleware from config
    let security_cfg = config.security.as_ref();
    let profile_str = security_cfg
        .map(|s| s.profile.as_str())
        .unwrap_or("balanced");
    let security_profile: SecurityProfile = profile_str.parse().unwrap_or_else(|_| {
        tracing::warn!(profile = %profile_str, "invalid security profile, using balanced");
        SecurityProfile::Balanced
    });
    let mut security_config = SecurityConfig::from_profile(security_profile);
    // Apply optional config overrides
    if let Some(cfg) = security_cfg {
        security_config.scan_outbound = cfg.scan_outbound;
        if !cfg.scanner_checks.is_empty() {
            security_config.scanner.checks = cfg.scanner_checks.clone();
        }
    }
    if let Ok(url) = std::env::var("CALCIFORGE_REMOTE_SCANNER_URL") {
        if !url.trim().is_empty() {
            let fail_closed = std::env::var("CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false);
            if security_config.scanner.checks.is_empty() {
                security_config.scanner.checks =
                    adversary_detector::ScannerConfig::default_checks();
            }
            let already_configured = security_config.scanner.checks.iter().any(|check| {
                matches!(
                    check,
                    adversary_detector::ScannerCheckConfig::RemoteHttp {
                        url: configured,
                        ..
                    } if configured == &url
                )
            });
            if !already_configured {
                security_config
                    .scanner
                    .checks
                    .push(adversary_detector::ScannerCheckConfig::RemoteHttp { url, fail_closed });
            }
        }
    }
    let scanner = AdversaryScanner::new(security_config.scanner.clone());
    let audit_logger = AuditLogger::new("calciforge");
    let channel_scanner = Arc::new(ChannelScanner::new(
        scanner,
        audit_logger,
        security_config.clone(),
    ));
    info!(
        profile = %security_profile,
        intercepted_tools = ?security_config.intercepted_tools,
        scan_outbound = security_config.scan_outbound,
        "adversary-detector middleware active"
    );

    let config = Arc::new(config);
    let router = Arc::new(Router::new());

    // Initialize model-gateway synthetic routing if configured.
    let has_synthetic_models = !config.alloys.is_empty()
        || !config.cascades.is_empty()
        || !config.dispatchers.is_empty()
        || !config.exec_models.is_empty();
    let alloy_manager = if !has_synthetic_models {
        None
    } else {
        match AlloyManager::from_gateway_configs(
            &config.alloys,
            &config.cascades,
            &config.dispatchers,
            &config.exec_models,
        ) {
            Ok(manager) => {
                info!(
                    alloys = config.alloys.len(),
                    cascades = config.cascades.len(),
                    dispatchers = config.dispatchers.len(),
                    exec_models = config.exec_models.len(),
                    "model gateway synthetic routing initialized"
                );
                Some(manager)
            }
            Err(e) => {
                error!(error = %e, "failed to initialize alloy manager");
                None
            }
        }
    };

    // Initialize local model manager early so CommandHandler and proxy both share it.
    let local_manager_early: Option<Arc<local_model::LocalModelManager>> =
        config.local_models.as_ref().and_then(|lm_cfg| {
            if lm_cfg.enabled {
                Some(Arc::new(local_model::LocalModelManager::new(
                    lm_cfg.clone(),
                )))
            } else {
                None
            }
        });

    let command_handler = {
        let handler = CommandHandler::new(config.clone());
        let handler = if let Some(manager) = alloy_manager {
            handler.with_alloy_manager(manager)
        } else {
            handler
        };
        let handler = if let Some(ref lm) = local_manager_early {
            handler.with_local_manager(Arc::clone(lm))
        } else {
            handler
        };
        Arc::new(handler)
    };

    // Detect enabled channels
    let has_telegram = config
        .channels
        .iter()
        .any(|c| c.kind == "telegram" && c.enabled);

    let has_matrix = config
        .channels
        .iter()
        .any(|c| c.kind == "matrix" && c.enabled);

    let has_whatsapp = config
        .channels
        .iter()
        .any(|c| c.kind == "whatsapp" && c.enabled);

    let has_signal = config
        .channels
        .iter()
        .any(|c| c.kind == "signal" && c.enabled);

    let has_sms = config.channels.iter().any(|c| c.kind == "sms" && c.enabled);

    let has_mock = config
        .channels
        .iter()
        .any(|c| c.kind == "mock" && c.enabled);

    let no_channels =
        !has_telegram && !has_matrix && !has_whatsapp && !has_signal && !has_sms && !has_mock;

    let proxy_enabled = config.proxy.as_ref().is_some_and(|p| p.enabled);

    if !args.proxy_only && no_channels {
        if proxy_enabled {
            warn!("no enabled channels found — running in proxy-only mode");
        } else {
            error!("no enabled channels found in config — nothing to do");
            std::process::exit(1);
        }
    }

    // Run enabled channels concurrently and fail fast if any enabled channel
    // exits with an error. Otherwise a dead channel can be masked by the
    // long-lived proxy task and systemd will report a healthy service.
    // Channels that are not enabled resolve immediately with Ok(()).
    let telegram_fut = async {
        if !args.proxy_only && has_telegram {
            info!("starting Telegram channel");
            channels::telegram::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
            )
            .await
            .context("Telegram channel error")
        } else {
            Ok(())
        }
    };

    let matrix_fut = async {
        if !args.proxy_only && has_matrix {
            info!("starting Matrix channel");
            channels::matrix::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
            )
            .await
            .context("Matrix channel error")
        } else {
            Ok(())
        }
    };

    let whatsapp_fut = async {
        if !args.proxy_only && has_whatsapp {
            info!("starting WhatsApp channel (embedded WhatsApp Web client)");
            channels::whatsapp::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
                channel_scanner.clone(),
            )
            .await
            .context("WhatsApp channel error")
        } else {
            Ok(())
        }
    };

    let signal_fut = async {
        if !args.proxy_only && has_signal {
            info!("starting Signal channel (embedded signal-cli-rest-api bridge)");
            channels::signal::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
                channel_scanner.clone(),
            )
            .await
            .context("Signal channel error")
        } else {
            Ok(())
        }
    };

    let sms_fut = async {
        if !args.proxy_only && has_sms {
            info!("starting Text/iMessage channel (Linq webhook receiver)");
            channels::sms::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
                channel_scanner.clone(),
            )
            .await
            .context("Text/iMessage channel error")
        } else {
            Ok(())
        }
    };

    let mock_fut = async {
        if !args.proxy_only && has_mock {
            info!("starting Mock channel");
            channels::mock::run(
                config.clone(),
                router.clone(),
                command_handler.clone(),
                context_store.clone(),
            )
            .await
            .context("Mock channel error")
        } else {
            Ok(())
        }
    };

    // Start proxy server if enabled.
    // local_manager_early was created above and shared with CommandHandler.
    let proxy_config = config.proxy.clone().unwrap_or_default();
    let proxy_enabled = proxy_config.enabled;

    // Auto-load startup model in background (if configured).
    if let Some(ref lm) = local_manager_early {
        if let Some(ref start_id) = config.local_models.as_ref().and_then(|c| c.current.clone()) {
            let id = start_id.clone();
            let mgr = Arc::clone(lm);
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || mgr.switch(&id)).await;
                match result {
                    Ok(Ok(loaded)) => info!(model = %loaded.id, "Auto-loaded startup local model"),
                    Ok(Err(e)) => error!(error = %e, "Failed to auto-load startup local model"),
                    Err(e) => error!(error = %e, "spawn_blocking panic auto-loading local model"),
                }
            });
        }
    }

    let local_manager = local_manager_early;

    let proxy_fut = async {
        if proxy_enabled {
            let alloy_mgr = command_handler
                .alloy_manager()
                .map(|m| Arc::new((*m).clone()))
                .unwrap_or_else(|| Arc::new(crate::providers::alloy::AlloyManager::empty()));
            let providers = Arc::new(crate::providers::ProviderRegistry::new());
            proxy::start_proxy_server(
                proxy_config,
                config.exec_models.clone(),
                alloy_mgr,
                providers,
                local_manager,
            )
            .await
            .context("Proxy server error")
        } else {
            Ok(())
        }
    };

    tokio::try_join!(
        telegram_fut,
        matrix_fut,
        whatsapp_fut,
        signal_fut,
        sms_fut,
        mock_fut,
        proxy_fut
    )?;

    Ok(())
}
