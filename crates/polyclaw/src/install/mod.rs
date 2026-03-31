//! PolyClaw multi-target installer.
//!
//! Configures PolyClaw to route messages to one or more downstream "claws"
//! (agent instances), each potentially on a different remote host.
//!
//! # Modes
//!
//! - **Interactive TUI** (default, no flags): launches a [`wizard::run_wizard`]
//!   step-by-step dialog using `dialoguer`.
//! - **Non-interactive CLI** (`--polyclaw-host` / `--claw` flags present):
//!   parses targets from flags and runs the install pipeline headlessly.
//!
//! # Architecture
//!
//! The key design axis is **remote configurability**, not adapter kind:
//!
//! | Adapter | Remote config via SSH? | What installer does |
//! |---------|----------------------|---------------------|
//! | `NzcNative` | ✅ | SSH in, read/backup/edit NZC config, health-check |
//! | `OpenClawHttp` | ✅ | SSH in, read/backup/edit `openclaw.json`, health-check |
//! | `OpenAiCompat` | ❌ | Record endpoint, health-check only |
//! | `Webhook` | ❌ | Record endpoint, health-check only |
//! | `Cli` | ❌ | Record command, no network health-check |
//!
//! # Safety invariants
//!
//! - Backup is always taken and verified before any remote config write.
//! - `--dry-run` prints all planned changes without touching anything.
//! - Health check after apply; automatic rollback on failure.
//! - One claw at a time — never mutate two claws in the same SSH session.

pub mod cli;
pub mod executor;
pub mod health;
pub mod json5;
pub mod migration_types;
pub mod model;
pub mod ssh;
pub mod wizard;

pub use cli::InstallArgs;
pub use model::{ClawKind, ClawTarget, InstallTarget, PolyClawTarget, WebhookFormat};

use anyhow::Result;
use tracing::info;

/// Entry point: parse args and dispatch to interactive or non-interactive path.
///
/// Called from `main.rs` when the `install` subcommand is detected.
pub async fn run(args: InstallArgs) -> Result<()> {
    if args.is_interactive() {
        info!("no CLI targets provided — launching interactive wizard");
        wizard::run_wizard().await
    } else {
        info!("CLI targets provided — running non-interactive install");
        let target = cli::parse_install_target(&args)?;
        executor::run_install(target, &args).await
    }
}
