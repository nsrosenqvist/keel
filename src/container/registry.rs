//! Composite [`Backend`] that fans out across multiple owners.
//!
//! The TUI / runtime each hold a single `Arc<dyn Backend>`. The
//! registry preserves that uniformity while letting projects mix
//! container services (compose) with non-container services (custom
//! shell-driven, systemd, …) in a single dashboard. Per-method
//! routing rules:
//!
//! - `status`, `tail_logs`, `service_action(verb, [name])`: route to
//!   the owning backend (custom backend wins for its declared names;
//!   anything else falls through to the container backend).
//! - `service_action(verb, [])`: project-wide. Runs every owner, with
//!   the container backend's `Child` returned to the caller as the
//!   primary stream and any custom backend running in a detached
//!   task whose outcome surfaces via `tracing`. Phase D simplification
//!   — adequate for the common case (one owner has services) and
//!   honest about the edge (logged not lost).
//! - `list_services`: union of all owners.
//! - `exec`, `passthrough`, `exec_with_stdin`: container backend
//!   only; the registry forwards.

use crate::container::custom::CustomBackend;
use crate::container::{Backend, BackendError, ExecOptions, ServiceStatus};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::process::Child;
use tracing::{debug, warn};

/// Composite backend. `container` is optional so projects with only
/// custom services (`[runtime] backend = "none"` plus `services.custom`
/// or `services.systemd`) still work.
pub struct ServiceRegistry {
    container: Option<Arc<dyn Backend>>,
    custom: Option<Arc<CustomBackend>>,
    /// Cached set of names owned by `custom` for O(1) routing.
    custom_names: HashSet<String>,
}

impl ServiceRegistry {
    pub fn new(container: Option<Arc<dyn Backend>>, custom: Option<CustomBackend>) -> Self {
        let (custom, custom_names) = match custom {
            Some(c) if !c.is_empty() => {
                let names: HashSet<String> = c.names().map(String::from).collect();
                (Some(Arc::new(c)), names)
            }
            _ => (None, HashSet::new()),
        };
        Self {
            container,
            custom,
            custom_names,
        }
    }

    fn route(&self, service: &str) -> Option<Arc<dyn Backend>> {
        if self.custom_names.contains(service)
            && let Some(c) = &self.custom
        {
            return Some(Arc::clone(c) as Arc<dyn Backend>);
        }
        self.container.as_ref().map(Arc::clone)
    }

    fn require_container(&self) -> Result<Arc<dyn Backend>, BackendError> {
        self.container.as_ref().map(Arc::clone).ok_or_else(|| {
            BackendError::Reported(
                "no container backend configured (set runtime.backend = \"compose\")".into(),
            )
        })
    }
}

#[async_trait]
impl Backend for ServiceRegistry {
    fn name(&self) -> &'static str {
        "registry"
    }

    async fn status(&self, service: &str) -> Result<ServiceStatus, BackendError> {
        match self.route(service) {
            Some(b) => b.status(service).await,
            None => Ok(ServiceStatus::Missing),
        }
    }

    async fn exec(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<i32, BackendError> {
        // Custom services don't support exec; route to the named
        // owner so the user gets the helpful "<name> is a custom
        // service" message rather than a generic compose error.
        match self.route(service) {
            Some(b) => b.exec(service, argv, opts).await,
            None => self.require_container()?.exec(service, argv, opts).await,
        }
    }

    async fn exec_with_stdin(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
        stdin: &str,
    ) -> Result<i32, BackendError> {
        match self.route(service) {
            Some(b) => b.exec_with_stdin(service, argv, opts, stdin).await,
            None => {
                self.require_container()?
                    .exec_with_stdin(service, argv, opts, stdin)
                    .await
            }
        }
    }

    async fn spawn_exec(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<Child, BackendError> {
        match self.route(service) {
            Some(b) => b.spawn_exec(service, argv, opts).await,
            None => {
                self.require_container()?
                    .spawn_exec(service, argv, opts)
                    .await
            }
        }
    }

    async fn spawn_exec_with_stdin(
        &self,
        service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<Child, BackendError> {
        match self.route(service) {
            Some(b) => b.spawn_exec_with_stdin(service, argv, opts).await,
            None => {
                self.require_container()?
                    .spawn_exec_with_stdin(service, argv, opts)
                    .await
            }
        }
    }

    async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError> {
        self.require_container()?.passthrough(args).await
    }

    async fn list_services(&self) -> Result<Vec<String>, BackendError> {
        let mut out = Vec::new();
        if let Some(b) = &self.container {
            out.extend(b.list_services().await?);
        }
        if let Some(c) = &self.custom {
            out.extend(c.list_services().await?);
        }
        // Stable ordering: container services first (by their natural
        // order from compose), then custom (also natural order). The
        // TUI sorts on insertion anyway; this is for callers that
        // care.
        Ok(out)
    }

    async fn service_action(&self, action: &str, services: &[&str]) -> Result<Child, BackendError> {
        // Targeted action — partition by owner and dispatch.
        if !services.is_empty() {
            return self.dispatch_targeted(action, services).await;
        }

        // Project-wide. Common cases:
        //   only container has services → forward
        //   only custom has services → forward
        //   both have services → primary = container, custom runs
        //                        detached (logged via tracing)
        let has_container = self.container.is_some();
        let has_custom = self.custom.is_some();
        match (has_container, has_custom) {
            (true, false) => {
                self.container
                    .as_ref()
                    .unwrap()
                    .service_action(action, &[])
                    .await
            }
            (false, true) => {
                self.custom
                    .as_ref()
                    .unwrap()
                    .service_action(action, &[])
                    .await
            }
            (true, true) => self.dispatch_all_combined(action).await,
            (false, false) => Err(BackendError::Reported(
                "no backends configured for service action".into(),
            )),
        }
    }

    async fn tail_logs(&self, service: &str) -> Result<Child, BackendError> {
        match self.route(service) {
            Some(b) => b.tail_logs(service).await,
            None => self.require_container()?.tail_logs(service).await,
        }
    }
}

impl ServiceRegistry {
    /// Targeted fan-out: split `services` into the subset owned by
    /// each backend, dispatch to whichever backend has work. Errors
    /// when both backends would need to act on the same call (we
    /// can't return two `Child`s from one trait call); the user
    /// hits up/restart per-service in that rare case.
    async fn dispatch_targeted(
        &self,
        action: &str,
        services: &[&str],
    ) -> Result<Child, BackendError> {
        let (custom_names, container_names): (Vec<&str>, Vec<&str>) = services
            .iter()
            .copied()
            .partition(|n| self.custom_names.contains(*n));

        if !custom_names.is_empty() && !container_names.is_empty() {
            return Err(BackendError::Reported(format!(
                "mixed-backend action `{action}` ({} custom + {} container services); \
                 invoke per-service or use the project-wide action",
                custom_names.len(),
                container_names.len(),
            )));
        }

        if !custom_names.is_empty() {
            return self
                .custom
                .as_ref()
                .expect("custom_names nonempty implies custom backend present")
                .service_action(action, &custom_names)
                .await;
        }
        // All targets belong to the container backend.
        self.require_container()?
            .service_action(action, &container_names)
            .await
    }

    /// Project-wide action across both owners. Returns the container
    /// backend's `Child` as the primary stream the TUI tails; the
    /// custom backend's action is detached and its outcome is logged.
    async fn dispatch_all_combined(&self, action: &str) -> Result<Child, BackendError> {
        let container = self.container.as_ref().expect("checked by caller");
        let custom = Arc::clone(self.custom.as_ref().expect("checked by caller"));
        let action_owned = action.to_string();

        // Spawn the container action first so its Child is what we
        // return to the caller. The custom action runs in parallel —
        // logged but not surfaced to the TUI run pane in phase D.
        let primary = container.service_action(action, &[]).await?;

        tokio::spawn(async move {
            match custom.service_action(&action_owned, &[]).await {
                Ok(mut child) => match child.wait().await {
                    Ok(status) => {
                        debug!(
                            action = %action_owned,
                            exit = ?status.code(),
                            "custom backend action completed"
                        );
                    }
                    Err(e) => {
                        warn!(action = %action_owned, error = %e, "custom backend action wait failed")
                    }
                },
                Err(e) => {
                    warn!(action = %action_owned, error = %e, "custom backend action spawn failed")
                }
            }
        });

        Ok(primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::custom::CustomEntry;
    use crate::container::null::NullBackend;
    use std::sync::Mutex;

    fn entry(name: &str, status: &str, start: &str, stop: &str) -> CustomEntry {
        CustomEntry {
            name: name.into(),
            status_cmd: status.into(),
            start_cmd: start.into(),
            stop_cmd: stop.into(),
            restart_cmd: format!("{stop} && {start}"),
            logs_cmd: None,
        }
    }

    /// Backend that records every call so tests can assert routing.
    struct RecordingBackend {
        name_str: &'static str,
        services: Vec<String>,
        log: Mutex<Vec<String>>,
    }

    impl RecordingBackend {
        fn new(name: &'static str, services: Vec<&str>) -> Self {
            Self {
                name_str: name,
                services: services.into_iter().map(String::from).collect(),
                log: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Backend for RecordingBackend {
        fn name(&self) -> &'static str {
            self.name_str
        }
        async fn status(&self, service: &str) -> Result<ServiceStatus, BackendError> {
            self.log.lock().unwrap().push(format!("status:{service}"));
            Ok(ServiceStatus::Running)
        }
        async fn exec(
            &self,
            service: &str,
            _argv: &[&str],
            _opts: &ExecOptions,
        ) -> Result<i32, BackendError> {
            self.log.lock().unwrap().push(format!("exec:{service}"));
            Ok(0)
        }
        async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("passthrough:{}", args.join(" ")));
            Ok(0)
        }
        async fn list_services(&self) -> Result<Vec<String>, BackendError> {
            Ok(self.services.clone())
        }
        async fn tail_logs(&self, service: &str) -> Result<Child, BackendError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("tail_logs:{service}"));
            // Spawn `true` to satisfy the Child return shape.
            tokio::process::Command::new("true")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(BackendError::Spawn)
        }
        async fn service_action(
            &self,
            action: &str,
            services: &[&str],
        ) -> Result<Child, BackendError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("action:{action}:{}", services.join(",")));
            tokio::process::Command::new("true")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(BackendError::Spawn)
        }
    }

    #[tokio::test]
    async fn list_services_unions_both_owners() {
        let container = Arc::new(RecordingBackend::new("rec", vec!["app", "worker"]));
        let custom = CustomBackend::new(vec![
            entry("ngrok", "true", ":", ":"),
            entry("postgres", "true", ":", ":"),
        ]);
        let registry = ServiceRegistry::new(Some(container), Some(custom));
        let mut listed = registry.list_services().await.unwrap();
        listed.sort();
        assert_eq!(listed, vec!["app", "ngrok", "postgres", "worker"]);
    }

    #[tokio::test]
    async fn status_routes_custom_to_custom_backend() {
        let container = Arc::new(RecordingBackend::new("rec", vec!["app"]));
        let custom = CustomBackend::new(vec![entry("ngrok", "true", ":", ":")]);
        let registry = ServiceRegistry::new(
            Some(Arc::clone(&container) as Arc<dyn Backend>),
            Some(custom),
        );
        registry.status("ngrok").await.unwrap();
        // Container backend never saw a status call for ngrok.
        assert!(container.calls().iter().all(|c| !c.contains("ngrok")));
    }

    #[tokio::test]
    async fn status_routes_compose_to_container_backend() {
        let container = Arc::new(RecordingBackend::new("rec", vec!["app"]));
        let custom = CustomBackend::new(vec![entry("ngrok", "true", ":", ":")]);
        let registry = ServiceRegistry::new(
            Some(Arc::clone(&container) as Arc<dyn Backend>),
            Some(custom),
        );
        registry.status("app").await.unwrap();
        assert!(container.calls().contains(&"status:app".to_string()));
    }

    #[tokio::test]
    async fn targeted_action_for_compose_only_routes_to_container() {
        let container = Arc::new(RecordingBackend::new("rec", vec!["app"]));
        let custom = CustomBackend::new(vec![entry("ngrok", "true", ":", ":")]);
        let registry = ServiceRegistry::new(
            Some(Arc::clone(&container) as Arc<dyn Backend>),
            Some(custom),
        );
        let _ = registry.service_action("up", &["app"]).await.unwrap();
        assert!(
            container
                .calls()
                .iter()
                .any(|c| c.starts_with("action:up:app"))
        );
    }

    #[tokio::test]
    async fn targeted_action_mixed_owners_errors_with_hint() {
        let container = Arc::new(RecordingBackend::new("rec", vec!["app"]));
        let custom = CustomBackend::new(vec![entry("ngrok", "true", ":", ":")]);
        let registry = ServiceRegistry::new(Some(container as Arc<dyn Backend>), Some(custom));
        let err = registry
            .service_action("up", &["app", "ngrok"])
            .await
            .unwrap_err();
        match err {
            BackendError::Reported(msg) => {
                assert!(msg.contains("mixed-backend"), "msg: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn project_wide_action_with_only_custom_uses_custom() {
        // No container backend at all — pure custom.
        let custom = CustomBackend::new(vec![entry("ngrok", "true", "true", "true")]);
        let registry = ServiceRegistry::new(None, Some(custom));
        let mut child = registry.service_action("up", &[]).await.unwrap();
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn passthrough_requires_container_backend() {
        let custom = CustomBackend::new(vec![entry("ngrok", "true", ":", ":")]);
        let registry = ServiceRegistry::new(None, Some(custom));
        let err = registry.passthrough(&["ps"]).await.unwrap_err();
        match err {
            BackendError::Reported(msg) => {
                assert!(msg.contains("no container backend"), "msg: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_registry_returns_missing_for_unknown_service() {
        let registry = ServiceRegistry::new(Some(Arc::new(NullBackend) as Arc<dyn Backend>), None);
        // Null backend's status returns Missing for any name, since it
        // forwards to that backend via routing.
        let s = registry.status("anything").await.unwrap();
        assert_eq!(s, ServiceStatus::Missing);
    }
}
