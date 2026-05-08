//! scaffl CLI entry point.
//!
//! This binary is a thin layer over `scaffl-runtime`: it parses flags,
//! discovers the project root, loads config, and hands off. Anything that
//! could be reused by the TUI lives in `scaffl-runtime`.

use anyhow::Result;
use clap::Parser;

mod app;
mod commands;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = app::Cli::parse();
    app::run(cli).await
}
