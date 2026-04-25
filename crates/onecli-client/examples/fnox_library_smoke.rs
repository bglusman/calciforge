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
    // FnoxLibrary::new() defers to upstream fnox::Fnox::discover()
    // which walks up from CWD finding fnox.toml + merging local +
    // parent + global configs.
    let fnox = FnoxLibrary::new();
    let names = fnox.list().await?;
    println!("declared secrets in active profile ({}):", names.len());
    for n in &names {
        println!("  {n}");
    }
    Ok(())
}
