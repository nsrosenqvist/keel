//! ampelos binary entry. Thin wrapper around [`ampelos::cli::run`].

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    ampelos::cli::run().await
}
