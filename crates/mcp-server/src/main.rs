//! `mcp-server` binary — wires the [`mcp_server::CalciforgeMcp`]
//! server to the rmcp stdio transport.
//!
//! Invoked by an agent's MCP client as a subprocess. Speaks JSON-RPC
//! over stdin/stdout. Never prints to stdout outside the MCP protocol
//! framing — diagnostic logging goes to stderr via `tracing`.

use mcp_server::CalciforgeMcp;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs to stderr — stdout is reserved for the MCP protocol stream.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mcp_server=info,secrets_client=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("mcp-server starting (stdio transport)");
    let server = CalciforgeMcp::default();
    server.serve(stdio()).await?.waiting().await?;
    tracing::info!("mcp-server shutting down");
    Ok(())
}
