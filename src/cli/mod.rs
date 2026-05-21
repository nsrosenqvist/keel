//! croft CLI surface.
//!
//! Bounded context: flag parsing, project discovery, and dispatch into the
//! runtime. Anything reused by the TUI lives in [`crate::runtime`] — this
//! module owns the command-line lifecycle only.

use anyhow::Result;
use clap::Parser;

pub mod app;
pub mod ci;
pub mod commands;
pub mod constants;
pub mod http;
pub mod update;

/// Parse argv and run the CLI to completion.
pub async fn run() -> Result<()> {
    let cli = app::Cli::parse();
    app::run(cli).await
}
