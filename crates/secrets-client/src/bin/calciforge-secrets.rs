//! `calciforge-secrets` — non-MCP secret discovery CLI.
//!
//! This exposes the same safe surface as the MCP server: list names and build
//! placeholder references. It never resolves or prints secret values.

use secrets_client::{FnoxClient, secret_reference_token};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => list().await,
        Some("ref") | Some("reference") => {
            let name = args
                .next()
                .ok_or_else(|| "usage: calciforge-secrets ref NAME".to_string())?;
            reference(&name)
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown command {other:?}\n\nRun `calciforge-secrets help`."
        )),
    }
}

async fn list() -> Result<(), String> {
    let names = FnoxClient::new()
        .list()
        .await
        .map_err(|e| format!("fnox list failed: {e}"))?;
    for name in names {
        println!("{name}");
    }
    Ok(())
}

fn reference(name: &str) -> Result<(), String> {
    let token = secret_reference_token(name).ok_or_else(|| {
        format!("invalid secret name {name:?}; allowed characters: A-Z a-z 0-9 _ -")
    })?;
    println!("{token}");
    Ok(())
}

fn print_help() {
    println!(
        "calciforge-secrets\n\
         \n\
         Safe secret discovery without MCP. Never prints values.\n\
         \n\
         Commands:\n\
           list       List stored fnox secret names\n\
           ref NAME   Print the canonical {{secret:NAME}} placeholder\n"
    );
}
