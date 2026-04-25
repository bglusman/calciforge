//! `zeroclawed-secret-paste` binary — CLI entry point for the paste
//! server. Useful for one-off command-line use (e.g., `... | xargs`),
//! and for testing the server flow without going through MCP.
//!
//! Reads `NAME` and optional `DESCRIPTION` from argv, prints the URL
//! to stdout, and runs the server until the request completes or
//! expires.

use std::io::{Write, stdout};
use tracing_subscriber::EnvFilter;
use zeroclawed_secret_paste::{PasteConfig, spawn_request};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zeroclawed_secret_paste=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut args = std::env::args().skip(1);
    let name = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: zeroclawed-secret-paste NAME [DESCRIPTION]"))?;
    let description = args.next().unwrap_or_default();

    let mut handle = spawn_request(
        name,
        description,
        onecli_client::FnoxClient::new(),
        PasteConfig::default(),
    )
    .await?;

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
