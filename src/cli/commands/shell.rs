//! `ampelos shell` — drop into an interactive shell.
//!
//! Two flavours:
//!
//! - **devcontainer** (default): inherits stdio, ensures the
//!   container is up, then `docker exec -it $name $SHELL`.
//! - **compose service** (`--service <name>`): `docker compose exec
//!   -it <name> $SHELL`. Independent of the devcontainer toggle —
//!   useful for projects that don't use devcontainers at all but still
//!   want a single command to drop into their `app` container.
//!
//! Stays out of recipe-execution territory: no env merging, no
//! profiles, no recipes. Just "give me a shell."

use crate::config::Config;
use crate::container::devcontainer::DevcontainerBackend;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::process::Command;

/// Entry point for `ampelos shell` and `ampelos shell --service <name>`.
pub async fn run(
    config: &Config,
    project_root: &Path,
    identity: &crate::runtime::Identity,
    service: Option<&str>,
) -> Result<i32> {
    if let Some(name) = service {
        return shell_into_service(name).await;
    }
    shell_into_devcontainer(config, project_root, identity).await
}

async fn shell_into_devcontainer(
    config: &Config,
    project_root: &Path,
    identity: &crate::runtime::Identity,
) -> Result<i32> {
    if !config.devcontainer.enabled {
        anyhow::bail!(
            "no shell target. Either enable `[devcontainer] enabled = true` in ampelos.toml, \
             or pass `--service <name>` to enter a compose service."
        );
    }
    let dc = crate::cli::app::build_devcontainer(config, project_root, identity)
        .context("build devcontainer backend")?
        .ok_or_else(|| anyhow::anyhow!("devcontainer disabled — this is a bug, please report"))?;

    dc.ensure_up()
        .await
        .context("ensure devcontainer is running")?;

    exec_into(&dc).await
}

async fn exec_into(dc: &Arc<DevcontainerBackend>) -> Result<i32> {
    // `docker exec -it <name> sh -c 'exec ${SHELL:-/bin/sh}'`: the
    // inner `exec` replaces sh with the user's preferred shell so
    // `ctrl+d` exits the container cleanly and exit codes pass
    // through.
    let mut cmd = Command::new("docker");
    cmd.args([
        "exec",
        "-it",
        dc.container_name(),
        "sh",
        "-c",
        "exec ${SHELL:-/bin/sh}",
    ]);
    let status = cmd
        .status()
        .await
        .context("spawn docker exec for devcontainer shell")?;
    Ok(status.code().unwrap_or(-1))
}

async fn shell_into_service(service: &str) -> Result<i32> {
    // Mirror `queue_service_attach`'s tmux create_with so the
    // out-of-TUI experience matches the in-TUI one.
    let mut cmd = Command::new("docker");
    cmd.args([
        "compose",
        "exec",
        "-it",
        service,
        "sh",
        "-c",
        "exec ${SHELL:-/bin/sh}",
    ]);
    let status = cmd
        .status()
        .await
        .with_context(|| format!("spawn docker compose exec for `{service}`"))?;
    Ok(status.code().unwrap_or(-1))
}
