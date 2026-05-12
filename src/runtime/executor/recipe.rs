//! Recipe-run orchestration.
//!
//! Public entry points (`run_recipe`, `run_script`), dependency
//! resolution (recipes ↔ scripts), and the sequential / parallel
//! step iterators sit here. Per-step execution lives in
//! [`super::step`]; this layer only decides *what* to run, not
//! *how* to spawn it.

use super::{BoxFut, Executor};
use crate::config::{Recipe, Run};
use crate::container::ServiceStatus;
use crate::runtime::error::RuntimeError;
use crate::runtime::ports::{RecipeProvider, ScriptProvider};
use std::collections::HashSet;
use tracing::instrument;

impl Executor {
    /// Run a recipe by name. `args` are forwarded to the recipe's `run` if
    /// `forward_args = true`.
    pub async fn run_recipe(&self, name: &str, args: &[String]) -> Result<i32, RuntimeError> {
        self.run_recipe_inner(name.to_string(), args.to_vec(), HashSet::new())
            .await
    }

    /// Run a script command by name. Mirrors [`Self::run_recipe`] but
    /// dispatches to a `.ampelos/commands/<name>` file.
    pub async fn run_script(&self, name: &str, args: &[String]) -> Result<i32, RuntimeError> {
        self.run_script_inner(name.to_string(), args.to_vec(), HashSet::new())
            .await
    }

    pub(crate) fn run_recipe_inner(
        &self,
        name: String,
        args: Vec<String>,
        mut in_progress: HashSet<String>,
    ) -> BoxFut<'_, Result<i32, RuntimeError>> {
        Box::pin(async move {
            if !in_progress.insert(name.clone()) {
                return Err(RuntimeError::DependencyCycle(name));
            }

            let raw_recipe =
                self.config
                    .get_recipe(&name)
                    .ok_or_else(|| RuntimeError::UnknownCommand {
                        name: name.clone(),
                        suggestion: None,
                    })?;
            let recipe = raw_recipe.with_profile(self.profile.as_deref());

            for dep in &recipe.needs {
                let code = self.run_dependency(&name, dep, in_progress.clone()).await?;
                if code != 0 {
                    return Ok(code);
                }
            }

            self.execute(&recipe, &args, in_progress).await
        })
    }

    pub(crate) fn run_script_inner(
        &self,
        name: String,
        args: Vec<String>,
        mut in_progress: HashSet<String>,
    ) -> BoxFut<'_, Result<i32, RuntimeError>> {
        Box::pin(async move {
            if !in_progress.insert(name.clone()) {
                return Err(RuntimeError::DependencyCycle(name));
            }
            let script =
                self.config
                    .get_script(&name)
                    .ok_or_else(|| RuntimeError::UnknownCommand {
                        name: name.clone(),
                        suggestion: None,
                    })?;
            for dep in &script.needs {
                let code = self.run_dependency(&name, dep, in_progress.clone()).await?;
                if code != 0 {
                    return Ok(code);
                }
            }
            self.execute_script(script, &args).await
        })
    }

    /// Run a dependency — looks for a recipe first, then a script, before
    /// erroring with [`RuntimeError::UnknownDependency`].
    pub(crate) async fn run_dependency(
        &self,
        from: &str,
        dep: &str,
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        if self.config.has_recipe(dep) {
            return self
                .run_recipe_inner(dep.to_string(), Vec::new(), in_progress)
                .await;
        }
        if self.config.has_script(dep) {
            return self
                .run_script_inner(dep.to_string(), Vec::new(), in_progress)
                .await;
        }
        Err(RuntimeError::UnknownDependency {
            recipe: from.to_string(),
            dep: dep.to_string(),
        })
    }

    #[instrument(skip(self, recipe, args, in_progress))]
    pub(crate) async fn execute(
        &self,
        recipe: &Recipe,
        args: &[String],
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        if let Some(service) = &recipe.service {
            let status = self.backend.status(service).await?;
            if status != ServiceStatus::Running {
                return Err(RuntimeError::Backend(
                    crate::container::BackendError::ServiceUnavailable {
                        service: service.clone(),
                        status: format!("{status:?}").to_lowercase(),
                    },
                ));
            }
        }

        match &recipe.run {
            Run::Single(cmd) => self.run_step(recipe, cmd, args, in_progress).await,
            Run::Steps(steps) if recipe.parallel => {
                self.run_steps_parallel(recipe, steps, args, in_progress)
                    .await
            }
            Run::Steps(steps) => {
                self.run_steps_sequential(recipe, steps, args, in_progress)
                    .await
            }
        }
    }

    async fn run_steps_sequential(
        &self,
        recipe: &Recipe,
        steps: &[String],
        args: &[String],
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        for (idx, step) in steps.iter().enumerate() {
            // Forward args only to the final step, mirroring `bash -c "a; b $@"`.
            let step_args: &[String] = if idx + 1 == steps.len() { args } else { &[] };
            let code = self
                .run_step(recipe, step, step_args, in_progress.clone())
                .await?;
            if code != 0 {
                return Ok(code);
            }
        }
        Ok(0)
    }

    async fn run_steps_parallel(
        &self,
        recipe: &Recipe,
        steps: &[String],
        args: &[String],
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        // Args forwarding has no useful semantics under parallelism — each
        // step runs concurrently, so "the last step" is undefined. We
        // forward to all steps when forward_args is set, matching how a
        // shell would do `cmd1 "$@" & cmd2 "$@" &`.
        let futures = steps.iter().map(|step| {
            let step_args = if recipe.forward_args {
                args.to_vec()
            } else {
                Vec::new()
            };
            let in_progress = in_progress.clone();
            let step = step.clone();
            async move { self.run_step(recipe, &step, &step_args, in_progress).await }
        });
        let results = futures::future::join_all(futures).await;

        // First Err wins; otherwise return the first non-zero exit, or 0.
        let mut first_failure: Option<i32> = None;
        for r in results {
            let code = r?;
            if code != 0 && first_failure.is_none() {
                first_failure = Some(code);
            }
        }
        Ok(first_failure.unwrap_or(0))
    }
}
