//! Container backend abstraction.
//!
//! Bounded context: anything that talks to a container runtime. The
//! [`Backend`] trait is the only thing the runtime crate depends on; concrete
//! backends are pluggable. v1 ships [`compose::ComposeBackend`]; podman /
//! plain-docker variants drop in behind the same trait without touching the
//! runtime.

pub mod compose;
pub mod custom;
pub mod devcontainer;
pub mod error;
pub mod null;
pub mod registry;

use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio::process::Child;

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

    /// Like [`Self::exec`], but feeds `stdin` to the child's standard
    /// input before waiting for it. Used for in-container script
    /// execution: scaffl pipes the script body into `<interpreter> -s
    /// -- <args>` rather than copying the file in.
    ///
    /// `tty` is forced off internally because compose `exec` rejects
    /// the combination of `-i` + piped stdin.
    async fn exec_with_stdin(
        &self,
        _service: &str,
        _argv: &[&str],
        _opts: &ExecOptions,
        _stdin: &str,
    ) -> Result<i32, BackendError> {
        Err(BackendError::Reported(
            "this backend does not support stdin-piped exec".into(),
        ))
    }

    /// Run a passthrough command directly against the backend (e.g.
    /// `docker compose ps`). Used by `compose_passthrough`.
    async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError>;

    /// Spawn a long-running process that tails logs for `service`. The
    /// returned [`Child`] has its stdout / stderr piped — callers wire
    /// them through their own line readers (e.g. an [`OutputSink`]
    /// channel for the TUI).
    ///
    /// Default implementation errors. Backends that don't have a tail
    /// notion (NullBackend, hypothetical future backends without log
    /// support) inherit this default.
    ///
    /// [`OutputSink`]: scaffl-runtime::OutputSink
    async fn tail_logs(&self, _service: &str) -> Result<Child, BackendError> {
        Err(BackendError::Reported(
            "no container backend configured (set containers.backend = \"compose\" in scaffl.toml)"
                .into(),
        ))
    }

    /// List the names of services known to this backend, e.g. by
    /// reading `docker-compose.yaml`. Used by the TUI to auto-populate
    /// service panes.
    ///
    /// Default implementation returns an empty list. Backends without a
    /// notion of services (NullBackend) inherit this.
    async fn list_services(&self) -> Result<Vec<String>, BackendError> {
        Ok(Vec::new())
    }

    /// Run a lifecycle action against zero or more services.
    ///
    /// Conventional actions: `"up"`, `"down"`, `"stop"`, `"restart"`.
    /// An empty `services` slice means "all services" (the natural
    /// docker compose default for these verbs). The returned [`Child`]
    /// has piped stdio so the TUI can stream its progress into a
    /// run-state buffer.
    async fn service_action(
        &self,
        _action: &str,
        _services: &[&str],
    ) -> Result<Child, BackendError> {
        Err(BackendError::Reported(
            "no container backend configured (set containers.backend = \"compose\")".into(),
        ))
    }
}

/// Conventional service-action verbs. Backends should accept these and
/// may accept additional verbs at their discretion.
pub mod service_action {
    pub const UP: &str = "up";
    pub const DOWN: &str = "down";
    pub const STOP: &str = "stop";
    pub const RESTART: &str = "restart";
}
