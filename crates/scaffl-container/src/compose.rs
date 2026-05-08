//! Docker Compose backend implementation.
//!
//! Shells out to `docker compose` (preferred) or `docker-compose` (fallback).
//! Detection is performed once at construction time; subsequent calls reuse
//! the discovered argv prefix.

use crate::{Backend, BackendError, ExecOptions, ServiceStatus};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

/// Compose driver discriminator: modern plugin (`docker compose`) vs legacy
/// standalone binary (`docker-compose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    Plugin,
    Standalone,
}

#[derive(Debug)]
pub struct ComposeBackend {
    driver: Driver,
}

impl ComposeBackend {
    /// Auto-detect the available compose driver. Prefers the plugin form.
    pub async fn detect() -> Result<Self, BackendError> {
        if Self::probe(&["docker", "compose", "version"]).await {
            debug!("compose driver: plugin");
            return Ok(Self {
                driver: Driver::Plugin,
            });
        }
        if which::which("docker-compose").is_ok() {
            debug!("compose driver: standalone");
            return Ok(Self {
                driver: Driver::Standalone,
            });
        }
        Err(BackendError::BinaryNotFound(
            "docker compose / docker-compose".into(),
        ))
    }

    /// Return the program + leading args used to invoke compose.
    fn prefix(&self) -> &'static [&'static str] {
        match self.driver {
            Driver::Plugin => &["docker", "compose"],
            Driver::Standalone => &["docker-compose"],
        }
    }

    async fn probe(argv: &[&str]) -> bool {
        let (head, tail) = match argv.split_first() {
            Some(parts) => parts,
            None => return false,
        };
        Command::new(head)
            .args(tail)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl Backend for ComposeBackend {
    fn name(&self) -> &'static str {
        match self.driver {
            Driver::Plugin => "compose",
            Driver::Standalone => "compose-legacy",
        }
    }

    async fn status(&self, service: &str) -> Result<ServiceStatus, BackendError> {
        // `compose ps -q <service>` outputs the container ID(s) for the
        // service, empty when not running.
        let prefix = self.prefix();
        let (head, tail) = prefix.split_first().expect("non-empty prefix");
        let mut cmd = Command::new(head);
        cmd.args(tail).args(["ps", "-q", service]);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = cmd.output().await?;
        if !output.status.success() {
            return Ok(ServiceStatus::Missing);
        }
        let stdout = std::str::from_utf8(&output.stdout)?.trim();
        if stdout.is_empty() {
            return Ok(ServiceStatus::Stopped);
        }
        Ok(ServiceStatus::Running)
    }

    async fn exec(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<i32, BackendError> {
        let prefix = self.prefix();
        let (head, tail) = prefix.split_first().expect("non-empty prefix");
        let mut cmd = Command::new(head);
        cmd.args(tail);
        cmd.arg("exec");
        if opts.tty {
            cmd.arg("-it");
        } else {
            cmd.arg("-T");
        }
        for (k, v) in &opts.env {
            cmd.arg("-e").arg(format!("{k}={v}"));
        }
        if let Some(wd) = &opts.workdir {
            cmd.arg("-w").arg(wd);
        }
        cmd.arg(service);
        cmd.args(argv);
        let status = cmd.status().await?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError> {
        let prefix = self.prefix();
        let (head, tail) = prefix.split_first().expect("non-empty prefix");
        let status = Command::new(head).args(tail).args(args).status().await?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn tail_logs(&self, service: &str) -> Result<tokio::process::Child, BackendError> {
        let prefix = self.prefix();
        let (head, tail) = prefix.split_first().expect("non-empty prefix");
        let mut cmd = Command::new(head);
        cmd.args(tail);
        // --tail seeds the buffer with recent history so users don't open
        // an empty pane on a long-running service. -f follows.
        cmd.args(["logs", "-f", "--tail", "200", service]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_prefix_matches_variant() {
        let plugin = ComposeBackend {
            driver: Driver::Plugin,
        };
        assert_eq!(plugin.prefix(), &["docker", "compose"]);

        let standalone = ComposeBackend {
            driver: Driver::Standalone,
        };
        assert_eq!(standalone.prefix(), &["docker-compose"]);
    }

    #[test]
    fn name_reflects_driver() {
        let plugin = ComposeBackend {
            driver: Driver::Plugin,
        };
        assert_eq!(plugin.name(), "compose");

        let standalone = ComposeBackend {
            driver: Driver::Standalone,
        };
        assert_eq!(standalone.name(), "compose-legacy");
    }
}
