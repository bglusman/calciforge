//! Smoke test for FnoxLibrary against a real fnox.toml. Run with:
//!
//! ```sh
//! cargo run --example fnox_library_smoke -p onecli-client --features fnox-library
//! ```
//!
//! Lists declared secret names in the active profile (default).
//! Skipped from the regular test suite because it depends on the
//! ambient fnox.toml — useful for one-off verification, not CI.

use onecli_client::FnoxLibrary;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let fnox = FnoxLibrary::discover(&cwd);
    println!("config root walk started from: {}", cwd.display());
    let names = fnox.list().await?;
    println!("declared secrets in default profile ({}):", names.len());
    for n in &names {
        println!("  {n}");
    }
    Ok(())
}
