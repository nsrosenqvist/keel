//! Per-step dispatch.
//!
//! `run_step` is the only function that decides whether a step
//! lands as a recipe / script reference, an in-service exec, a
//! devcontainer exec, or a host spawn. The four branches are
//! self-contained here; the host-spawn branch delegates to
//! [`super::host`] for the actual `tokio::process::Command` work.

use super::{Executor, WorkspaceTarget};
use crate::config::Recipe;
use crate::container::{Backend, ExecOptions};
use crate::runtime::error::RuntimeError;
use std::collections::HashSet;
use tokio::process::Command;
use tracing::debug;

impl Executor {
    pub(crate) async fn run_step(
        &self,
        recipe: &Recipe,
        step: &str,
        forwarded: &[String],
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        // A step is a recipe / script reference if it contains no whitespace
        // and names a known recipe or script.
        if !step.chars().any(char::is_whitespace) {
            if self.config.commands.contains_key(step) {
                debug!(recipe_ref = step, "step is recipe reference");
                return self
                    .run_recipe_inner(step.to_string(), forwarded.to_vec(), in_progress)
                    .await;
            }
            if self.config.scripts.contains_key(step) {
                debug!(script_ref = step, "step is script reference");
                return self
                    .run_script_inner(step.to_string(), forwarded.to_vec(), in_progress)
                    .await;
            }
        }

        let env = self.effective_env(Some(&recipe.env)).await?;

        let mut argv = shell_words::split(step).map_err(|e| RuntimeError::ArgvParse {
            input: step.into(),
            message: e.to_string(),
        })?;
        if recipe.forward_args {
            argv.extend(forwarded.iter().cloned());
        }
        if argv.is_empty() {
            return Ok(0);
        }

        if let Some(service) = &recipe.service {
            let opts = ExecOptions {
                tty: recipe.tty,
                env: env.project_only_map(),
                workdir: None,
            };
            let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            if self.sink.capture() {
                let child = self.backend.spawn_exec(service, &argv_refs, &opts).await?;
                return self.stream_child_to_sink(child).await;
            }
            return self
                .backend
                .exec(service, &argv_refs, &opts)
                .await
                .map_err(RuntimeError::from);
        }

        if let WorkspaceTarget::Devcontainer(dc) = &self.workspace {
            let opts = ExecOptions {
                tty: recipe.tty,
                env: env.project_only_map(),
                workdir: None,
            };
            let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            // Pass the container name as `service` — DevcontainerBackend
            // ignores the arg but `Backend::exec`'s signature requires
            // a name. ensure_up runs inside the backend itself.
            if self.sink.capture() {
                let child = dc
                    .spawn_exec(dc.container_name(), &argv_refs, &opts)
                    .await?;
                return self.stream_child_to_sink(child).await;
            }
            return dc
                .exec(dc.container_name(), &argv_refs, &opts)
                .await
                .map_err(RuntimeError::from);
        }

        // Host execution.
        let (program, rest) = argv.split_first().expect("non-empty argv");
        let mut cmd = Command::new(program);
        cmd.args(rest);
        cmd.current_dir(self.project_root.as_ref());
        env.apply_to(&mut cmd);
        self.spawn_host(cmd).await
    }
}
