//! `croft update` — replace the running binary with the latest release.

use anyhow::{Context, Result};

use crate::cli::update;

pub async fn run(force: bool, prerelease: bool) -> Result<()> {
    update::run_update(force, prerelease)
        .await
        .context("self-update failed")
}
