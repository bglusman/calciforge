//! `zeroclawed-mcp` binary — wires the [`zeroclawed_mcp::ZeroclawedMcp`]
//! server to the rmcp stdio transport.
//!
//! Invoked by an agent's MCP client as a subprocess. Speaks JSON-RPC
//! over stdin/stdout. Never prints to stdout outside the MCP protocol
//! framing — diagnostic logging goes to stderr via `tracing`.

use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;
use zeroclawed_mcp::ZeroclawedMcp;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs to stderr — stdout is reserved for the MCP protocol stream.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zeroclawed_mcp=info,onecli_client=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("zeroclawed-mcp starting (stdio transport)");
    let server = ZeroclawedMcp::default();
    server.serve(stdio()).await?.waiting().await?;
    tracing::info!("zeroclawed-mcp shutting down");
    Ok(())
}
