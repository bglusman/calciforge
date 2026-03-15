//! PolyClaw v2 — Rust agent gateway
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
mod hooks;
mod router;

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use crate::{
    commands::CommandHandler,
    config::load_config,
    context::ContextStore,
    router::Router,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing — respects RUST_LOG env var
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("polyclaw=info".parse()?))
        .init();

    info!("PolyClaw v2 starting");

    // Load config
    let config_path = config::config_path()?;
    info!(path = %config_path.display(), "loading config");
    let config = load_config().with_context(|| {
        format!(
            "Failed to load config from {}. Create it first (see README).",
            config_path.display()
        )
    })?;

    info!(
        version = config.polyclaw.version,
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

    let context_store = ContextStore::new(
        config.context.buffer_size,
        config.context.inject_depth,
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

    if !has_telegram && !has_matrix && !has_whatsapp {
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
            )
            .await
            .context("WhatsApp channel error")
        } else {
            Ok(())
        }
    };

    let (tg_result, mx_result, wa_result) = tokio::join!(telegram_fut, matrix_fut, whatsapp_fut);
    tg_result?;
    mx_result?;
    wa_result?;

    Ok(())
}
