//! croft binary entry. Thin wrapper around [`croft::cli::run`].

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    croft::cli::run().await
}
