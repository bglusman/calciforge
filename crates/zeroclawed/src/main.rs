//! ZeroClawed — Rust agent gateway
//!
//! Entry point: loads config, starts Telegram bot, routes messages to
//! the downstream OpenClaw HTTP agent.

#![recursion_limit = "512"]

mod adapters;
mod auth;
mod channels;
mod commands;
mod config;
mod context;
#[cfg(test)]
mod hooks;
#[cfg(test)]
mod install;
mod router;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use adversary_detector::audit::AuditLogger;
use adversary_detector::middleware::ChannelScanner;
use adversary_detector::profiles::{SecurityConfig, SecurityProfile};
use adversary_detector::scanner::AdversaryScanner;

use crate::{commands::CommandHandler, context::ContextStore, router::Router};

/// ZeroClawed — Rust agent gateway
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to config file (default: ~/.zeroclawed/config.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI args
    let args = Args::parse();

    // Initialize tracing — respects RUST_LOG env var
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("zeroclawed=info".parse()?))
        .init();

    info!("ZeroClawed starting");

    // Load config (from CLI arg or default path)
    let config_path = args
        .config
        .unwrap_or_else(|| config::config_path().expect("Failed to determine default config path"));
    info!(path = %config_path.display(), "loading config");
    let config = config::load_config_from(&config_path).with_context(|| {
        format!(
            "Failed to load config from {}. Create it first (see README).",
            config_path.display()
        )
    })?;

    info!(
        version = config.zeroclawed.version,
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

    let context_store = ContextStore::new(config.context.buffer_size, config.context.inject_depth);

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
    }
    let scanner = AdversaryScanner::new(security_config.scanner.clone());
    let audit_logger = AuditLogger::new("zeroclawed");
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
    let command_handler = Arc::new(CommandHandler::new(config.clone()));

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

    if !has_telegram && !has_matrix && !has_whatsapp && !has_signal {
        error!("no enabled channels found in config — nothing to do");
        std::process::exit(1);
    }

    // Run enabled channels concurrently via tokio::join!
    // Channels that are not enabled resolve immediately with Ok(()).
    let telegram_fut = async {
        if has_telegram {
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
        if has_matrix {
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
        if has_whatsapp {
            info!("starting WhatsApp channel (webhook receiver)");
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
        if has_signal {
            info!("starting Signal channel (webhook receiver)");
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

    let (tg_result, mx_result, wa_result, sig_result) =
        tokio::join!(telegram_fut, matrix_fut, whatsapp_fut, signal_fut);
    tg_result?;
    mx_result?;
    wa_result?;
    sig_result?;

    Ok(())
}
