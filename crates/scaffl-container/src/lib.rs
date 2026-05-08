//! Container backend abstraction.
//!
//! Bounded context: anything that talks to a container runtime. The
//! [`Backend`] trait is the only thing the runtime crate depends on; concrete
//! backends are pluggable. v1 ships [`compose::ComposeBackend`]; podman /
//! plain-docker variants drop in behind the same trait without touching the
//! runtime.

pub mod compose;
pub mod error;

use async_trait::async_trait;
use std::collections::BTreeMap;

pub use error::BackendError;

/// Whether a container is currently running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Missing,
}

/// Options for an `exec` invocation.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Allocate a TTY (interactive). Defaults to non-interactive (`-T`).
    pub tty: bool,
    /// Per-invocation environment overrides forwarded to the container.
    pub env: BTreeMap<String, String>,
    /// Optional working directory inside the container.
    pub workdir: Option<String>,
}

/// The contract a container backend must satisfy.
///
/// Methods are intentionally minimal — the runtime only needs status checks
/// and exec; lifecycle (`up`/`down`) is treated as a passthrough that the
/// CLI surfaces directly without runtime semantics.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Backend name, used in diagnostics (e.g. `"compose"`).
    fn name(&self) -> &'static str;

    /// Returns the running status of `service`.
    async fn status(&self, service: &str) -> Result<ServiceStatus, BackendError>;

    /// Execute `argv` inside `service`. The first element is the program;
    /// the rest are its arguments. The backend is responsible for shell
    /// quoting.
    ///
    /// Returns the process exit code. stdout/stderr are inherited from the
    /// parent — the runtime captures them via the supervised process tree
    /// when needed.
    async fn exec(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<i32, BackendError>;

    /// Run a passthrough command directly against the backend (e.g.
    /// `docker compose ps`). Used by `compose_passthrough`.
    async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError>;
}
