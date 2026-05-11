//! No-op backend.
//!
//! Used when `backend = "none"`, or as a fallback that lets the TUI / CLI
//! initialize before any real backend is required. All container
//! operations fail with a clear "backend = none" message.

use crate::{Backend, BackendError, ExecOptions, ServiceStatus};
use async_trait::async_trait;

#[derive(Debug, Default, Clone, Copy)]
pub struct NullBackend;

#[async_trait]
impl Backend for NullBackend {
    fn name(&self) -> &'static str {
        "none"
    }

    async fn status(&self, _service: &str) -> Result<ServiceStatus, BackendError> {
        Ok(ServiceStatus::Missing)
    }

    async fn exec(
        &self,
        service: &str,
        _argv: &[&str],
        _opts: &ExecOptions,
    ) -> Result<i32, BackendError> {
        Err(BackendError::Reported(format!(
            "backend = \"none\"; cannot exec in service `{service}` — set containers.backend = \"compose\" to enable container exec"
        )))
    }

    async fn passthrough(&self, _args: &[&str]) -> Result<i32, BackendError> {
        Err(BackendError::Reported(
            "backend = \"none\"; container passthrough is unavailable".into(),
        ))
    }

    async fn tail_logs(&self, _service: &str) -> Result<tokio::process::Child, BackendError> {
        Err(BackendError::Reported(
            "backend = \"none\"; declare a service in [[ui.pane]] only after setting containers.backend = \"compose\""
                .into(),
        ))
    }

    async fn service_action(
        &self,
        _action: &str,
        _services: &[&str],
    ) -> Result<tokio::process::Child, BackendError> {
        Err(BackendError::Reported(
            "backend = \"none\"; set containers.backend = \"compose\" to start / stop services"
                .into(),
        ))
    }

    // list_services inherits the default empty Vec — Null has nothing to enumerate.
}
