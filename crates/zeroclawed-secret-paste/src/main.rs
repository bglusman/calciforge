//! `zeroclawed-secret-paste` binary — CLI entry point for the paste
//! server. Useful for one-off command-line use (e.g., `... | xargs`),
//! and for testing the server flow without going through MCP.
//!
//! Two modes:
//!   - Single (default): `zeroclawed-secret-paste NAME [DESCRIPTION]`
//!     — one secret, single-line input.
//!   - Bulk: `zeroclawed-secret-paste --bulk LABEL [DESCRIPTION]`
//!     — multi-line `.env` dump, per-key result page.

use std::io::{Write, stdout};
use tracing_subscriber::EnvFilter;
use zeroclawed_secret_paste::{PasteConfig, spawn_bulk_request, spawn_request};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zeroclawed_secret_paste=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let bulk = args.first().is_some_and(|a| a == "--bulk");
    if bulk {
        args.remove(0);
    }
    let name_or_label = args.first().cloned().ok_or_else(|| {
        anyhow::anyhow!("usage: zeroclawed-secret-paste [--bulk] NAME [DESCRIPTION]")
    })?;
    let description = args.get(1).cloned().unwrap_or_default();

    // Demo escape hatch: PASTE_INSECURE_NO_ORIGIN=1 disables the
    // localhost-Origin check so a phone on the LAN can submit. NOT
    // safe for any deployment with real secrets — this is for showing
    // the UI from another device on a trusted network.
    let insecure_no_origin = std::env::var("PASTE_INSECURE_NO_ORIGIN").as_deref() == Ok("1");
    let cfg = PasteConfig {
        require_localhost_origin: !insecure_no_origin,
        ..PasteConfig::default()
    };
    let mut handle = if bulk {
        spawn_bulk_request(
            name_or_label,
            description,
            onecli_client::FnoxClient::new(),
            cfg,
        )
        .await?
    } else {
        spawn_request(
            name_or_label,
            description,
            onecli_client::FnoxClient::new(),
            cfg,
        )
        .await?
    };

    // URL goes to stdout so it can be piped/echoed cleanly.
    println!("{}", handle.url);
    stdout().flush()?;
    eprintln!("Open the URL above in a browser. Server will exit on submit or 5-min expiry.");

    // Race submission against expiry. Whichever fires first wins; on
    // submit we trigger graceful shutdown so axum drains in-flight
    // requests (the confirmation page render in particular) before the
    // process exits.
    let until_expiry = (handle.expires_at - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();

    tokio::select! {
        result = handle.wait_submitted() => {
            match result {
                Ok(()) => eprintln!("Submitted. Shutting down."),
                Err(()) => eprintln!("Server stopped before submission."),
            }
        }
        _ = tokio::time::sleep(until_expiry) => {
            eprintln!("Expired without submission.");
        }
    }

    handle.shutdown();
    Ok(())
}
