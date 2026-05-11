//! keel binary entry. Thin wrapper around [`keel::cli::run`].

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    keel::cli::run().await
}
