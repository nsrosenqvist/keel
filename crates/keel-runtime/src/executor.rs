//! Recipe executor.
//!
//! Turns a recipe (already resolved by the [`Resolver`](crate::Resolver))
//! into actual process work, talking to a [`keel_container::Backend`] for
//! anything container-shaped. The CLI and the TUI both call into this; both
//! get the same semantics.
//!
//! Bounded responsibility: receive inputs, produce exit codes. Does not
//! decide *which* recipe to run — that's the resolver's job — and does not
//! parse anything from disk — that's the config's job.

use crate::env::Env;
use crate::error::RuntimeError;
use crate::sink::{InheritSink, OutputSink, OutputStream};
use crate::worktree::Identity;
use keel_config::{Config, Recipe, Run, ScriptCommand};
use keel_container::devcontainer::DevcontainerBackend;
use keel_container::{Backend, ExecOptions, ServiceStatus};
use std::collections::HashSet;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::OnceCell;
use tracing::{debug, instrument};

type BoxFut<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// The executor.
///
/// Holds shared, immutable references to its collaborators. Cheap to clone
/// for spawning concurrent recipe steps; the cached base env is shared via
/// [`Arc`].
/// Where "host" execution lands when a recipe / script has no
/// `in = "<service>"`. `Local` is the historical default (fork on
/// the user's machine); `Devcontainer` routes through a docker-managed
/// workspace container described by the project's `devcontainer.json`.
///
/// Separate from the container [`Backend`] abstraction (which handles
/// `in = "<service>"` routing for compose/podman/custom) — the two
/// dimensions are orthogonal and v1's opt-in devcontainer leaves the
/// existing service-routing path untouched.
#[derive(Clone)]
pub enum WorkspaceTarget {
    Local,
    Devcontainer(Arc<DevcontainerBackend>),
}

#[derive(Clone)]
pub struct Executor {
    backend: Arc<dyn Backend>,
    config: Arc<Config>,
    project_root: Arc<Path>,
    base_env: Arc<OnceCell<Env>>,
    sink: Arc<dyn OutputSink>,
    /// Active profile name, applied to recipes via [`Recipe::with_profile`].
    profile: Option<String>,
    /// Pre-detected worktree identity. When `None`, [`Env::resolve`]
    /// auto-detects on first use. CLI / TUI pass a known identity to
    /// avoid duplicate `git rev-parse` invocations.
    identity: Option<Identity>,
    /// Where no-`in` work lands. Defaults to [`WorkspaceTarget::Local`].
    workspace: WorkspaceTarget,
}

impl Executor {
    pub fn new(backend: Arc<dyn Backend>, config: Arc<Config>, project_root: &Path) -> Self {
        Self {
            backend,
            config,
            project_root: Arc::from(project_root),
            base_env: Arc::new(OnceCell::new()),
            sink: Arc::new(InheritSink),
            profile: None,
            identity: None,
            workspace: WorkspaceTarget::Local,
        }
    }

    /// Return a clone that routes no-`in` recipes / scripts into the
    /// devcontainer. The caller is responsible for having validated
    /// that `[devcontainer] enabled = true` and the spec was parsed
    /// successfully — at this point the backend is just plumbed
    /// through.
    pub fn with_devcontainer(&self, devcontainer: Arc<DevcontainerBackend>) -> Self {
        let mut clone = self.clone();
        clone.workspace = WorkspaceTarget::Devcontainer(devcontainer);
        clone
    }

    pub fn workspace_target(&self) -> &WorkspaceTarget {
        &self.workspace
    }

    /// Return a clone with a pre-detected worktree identity. Skips the
    /// implicit `git rev-parse` calls inside `Env::resolve`.
    pub fn with_identity(&self, identity: Identity) -> Self {
        let mut clone = self.clone();
        clone.identity = Some(identity);
        clone
    }

    /// Return a clone of this executor that uses `sink` for output capture
    /// instead of inheriting stdio. Useful for the TUI, where each pane's
    /// output is streamed into a per-pane buffer.
    pub fn with_sink(&self, sink: Arc<dyn OutputSink>) -> Self {
        let mut clone = self.clone();
        clone.sink = sink;
        clone
    }

    /// Activate a named profile for subsequent recipe runs. Recipes that
    /// declare `[command.<name>.profile.<profile>]` overrides will have
    /// those overrides applied.
    pub fn with_profile(&self, profile: impl Into<String>) -> Self {
        let mut clone = self.clone();
        clone.profile = Some(profile.into());
        clone
    }

    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Resolve and cache the project base env (process + .env + `[env]`).
    /// Subsequent calls return the cached value without re-running
    /// `from_command`s.
    async fn base_env(&self) -> Result<&Env, RuntimeError> {
        self.base_env
            .get_or_try_init(|| async {
                match &self.identity {
                    Some(id) => {
                        Env::resolve_with_identity(&self.config, self.project_root.as_ref(), id)
                            .await
                    }
                    None => Env::resolve(&self.config, self.project_root.as_ref()).await,
                }
            })
            .await
    }

    /// The env handed to a single spawn: cached base env + per-step
    /// overrides. Single place every spawn path goes through, so the
    /// merge rule (recipe/script-level `env = {...}` overrides win
    /// over the base) is asserted in exactly one location.
    async fn effective_env(
        &self,
        overrides: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<Env, RuntimeError> {
        let env = self.base_env().await?.clone();
        match overrides {
            Some(o) => Ok(env.with_overrides(o.iter().map(|(k, v)| (k.as_str(), v.as_str())))),
            None => Ok(env),
        }
    }

    /// Run a recipe by name. `args` are forwarded to the recipe's `run` if
    /// `forward_args = true`.
    pub async fn run_recipe(&self, name: &str, args: &[String]) -> Result<i32, RuntimeError> {
        self.run_recipe_inner(name.to_string(), args.to_vec(), HashSet::new())
            .await
    }

    /// Run a script command by name. Mirrors [`Self::run_recipe`] but
    /// dispatches to a `.keel/commands/<name>` file.
    pub async fn run_script(&self, name: &str, args: &[String]) -> Result<i32, RuntimeError> {
        self.run_script_inner(name.to_string(), args.to_vec(), HashSet::new())
            .await
    }

    fn run_recipe_inner(
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
                    .commands
                    .get(&name)
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

    fn run_script_inner(
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
                    .scripts
                    .get(&name)
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
    async fn run_dependency(
        &self,
        from: &str,
        dep: &str,
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        if self.config.commands.contains_key(dep) {
            return self
                .run_recipe_inner(dep.to_string(), Vec::new(), in_progress)
                .await;
        }
        if self.config.scripts.contains_key(dep) {
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
    async fn execute(
        &self,
        recipe: &Recipe,
        args: &[String],
        in_progress: HashSet<String>,
    ) -> Result<i32, RuntimeError> {
        if let Some(service) = &recipe.service {
            let status = self.backend.status(service).await?;
            if status != ServiceStatus::Running {
                return Err(RuntimeError::Backend(
                    keel_container::BackendError::ServiceUnavailable {
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

    async fn run_step(
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
                let child = self
                    .backend
                    .spawn_exec(service, &argv_refs, &opts)
                    .await?;
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

    /// Run a [`Command`] on the host, honouring the configured
    /// [`OutputSink`]: pipe-and-stream when the sink wants capture, or
    /// inherit-and-await when it doesn't.
    async fn spawn_host(&self, mut cmd: Command) -> Result<i32, RuntimeError> {
        if !self.sink.capture() {
            let status = cmd
                .status()
                .await
                .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
            return Ok(status.code().unwrap_or(-1));
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        // When the consumer (TUI) drops the spawning task, the Child
        // is dropped, and kill_on_drop fires SIGKILL. Without this,
        // aborting the JoinHandle would leak the process.
        cmd.kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
        self.stream_child_to_sink(child).await
    }

    /// Write `body` into the child's piped stdin (closing it on EOF
    /// so `bash -s` / `sh -s` start) and stream the rest through the
    /// configured sink. Used for the in-container script exec path
    /// when the TUI sink wants line-by-line capture.
    async fn write_stdin_and_stream(
        &self,
        mut child: tokio::process::Child,
        body: &str,
    ) -> Result<i32, RuntimeError> {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin_handle) = child.stdin.take() {
            stdin_handle
                .write_all(body.as_bytes())
                .await
                .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
            drop(stdin_handle);
        }
        self.stream_child_to_sink(child).await
    }

    /// Pump a piped-stdio [`tokio::process::Child`] through the
    /// configured sink and await its exit code. Shared between host
    /// exec (where we own the spawn) and container exec (where the
    /// backend hands us the already-spawned [`Child`]).
    async fn stream_child_to_sink(
        &self,
        mut child: tokio::process::Child,
    ) -> Result<i32, RuntimeError> {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_task = stdout.map(|s| {
            let sink = Arc::clone(&self.sink);
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink.write_line(OutputStream::Stdout, &line);
                }
            })
        });
        let stderr_task = stderr.map(|s| {
            let sink = Arc::clone(&self.sink);
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink.write_line(OutputStream::Stderr, &line);
                }
            })
        });

        let status = child
            .wait()
            .await
            .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
        if let Some(t) = stdout_task {
            let _ = t.await;
        }
        if let Some(t) = stderr_task {
            let _ = t.await;
        }
        Ok(status.code().unwrap_or(-1))
    }

    async fn execute_script(
        &self,
        script: &ScriptCommand,
        args: &[String],
    ) -> Result<i32, RuntimeError> {
        // Scripts get two extra env vars on top of the base resolution
        // chain: KEEL_PROJECT_DIR (worktree project root) and
        // KEEL_SCRIPT_DIR (the script file's parent directory).
        // They land *between* the base env and the script's own
        // `env = {...}`, so user overrides still win — but the
        // common case of "I just want to know where my script
        // lives" works without `dirname "$0"` boilerplate. Both
        // values are host-side paths even for in-container scripts;
        // ignore them inside the container if irrelevant.
        let project_dir = self.project_root.display().to_string();
        let script_dir = script
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| project_dir.clone());
        let env = self
            .base_env()
            .await?
            .clone()
            .with_overrides([
                ("KEEL_PROJECT_DIR", project_dir.as_str()),
                ("KEEL_SCRIPT_DIR", script_dir.as_str()),
            ])
            .with_overrides(script.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        // In-container script: pipe the script body to
        // `<interpreter> -s -- <args>` over the backend's stdin-piped
        // exec. The shebang (if any) drives the interpreter choice;
        // default is `sh`.
        if let Some(service) = &script.service {
            let body = std::fs::read_to_string(&script.path)
                .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
            let interpreter = parse_shebang_interpreter(&body).unwrap_or("sh");

            // Service must be running before we exec into it.
            let status = self.backend.status(service).await?;
            if status != ServiceStatus::Running {
                return Err(RuntimeError::Backend(
                    keel_container::BackendError::ServiceUnavailable {
                        service: service.clone(),
                        status: format!("{status:?}").to_lowercase(),
                    },
                ));
            }

            let mut argv: Vec<&str> = vec![interpreter, "-s"];
            if script.forward_args && !args.is_empty() {
                argv.push("--");
                argv.extend(args.iter().map(String::as_str));
            }
            let opts = ExecOptions {
                tty: false, // forced off — stdin pipe + TTY are mutually exclusive
                env: env.project_only_map(),
                workdir: None,
            };
            if self.sink.capture() {
                let child = self
                    .backend
                    .spawn_exec_with_stdin(service, &argv, &opts)
                    .await?;
                return self.write_stdin_and_stream(child, &body).await;
            }
            return self
                .backend
                .exec_with_stdin(service, &argv, &opts, &body)
                .await
                .map_err(RuntimeError::from);
        }

        if let WorkspaceTarget::Devcontainer(dc) = &self.workspace {
            // Mirror the in-service script path: pipe the script body
            // to `<interpreter> -s -- <args>` inside the devcontainer.
            // The container needs to be up first; the backend handles
            // ensure_up on `exec_with_stdin`.
            let body = std::fs::read_to_string(&script.path)
                .map_err(|e| RuntimeError::Backend(keel_container::BackendError::Spawn(e)))?;
            let interpreter = parse_shebang_interpreter(&body).unwrap_or("sh");
            let mut argv: Vec<&str> = vec![interpreter, "-s"];
            if script.forward_args && !args.is_empty() {
                argv.push("--");
                argv.extend(args.iter().map(String::as_str));
            }
            let opts = ExecOptions {
                tty: false,
                env: env.project_only_map(),
                workdir: None,
            };
            if self.sink.capture() {
                let child = dc
                    .spawn_exec_with_stdin(dc.container_name(), &argv, &opts)
                    .await?;
                return self.write_stdin_and_stream(child, &body).await;
            }
            return dc
                .exec_with_stdin(dc.container_name(), &argv, &opts, &body)
                .await
                .map_err(RuntimeError::from);
        }

        let mut cmd = Command::new(&script.path);
        if script.forward_args {
            cmd.args(args);
        }
        cmd.current_dir(self.project_root.as_ref());
        env.apply_to(&mut cmd);
        self.spawn_host(cmd).await
    }

    /// Run a passthrough through the backend (e.g. `compose ps`).
    pub async fn passthrough(&self, args: &[&str]) -> Result<i32, RuntimeError> {
        self.backend.passthrough(args).await.map_err(Into::into)
    }

    /// Exec a raw command in a service (used by service passthrough).
    pub async fn service_exec(
        &self,
        service: &str,
        argv: &[&str],
        tty: bool,
    ) -> Result<i32, RuntimeError> {
        let env = self.effective_env(None).await?;
        let opts = ExecOptions {
            tty,
            env: env.project_only_map(),
            workdir: None,
        };
        self.backend
            .exec(service, argv, &opts)
            .await
            .map_err(Into::into)
    }
}

/// Read the shebang (if any) of a script body and return a plain
/// interpreter name suitable for use inside a container.
///
/// Recognises the common cases — `bash`, `zsh`, `sh` — by substring
/// match against the shebang line. Anything else (including `python`,
/// `node`, etc.) returns `None`; the caller will fall back to `sh`.
/// This is intentionally narrow: containers usually have at least
/// `sh` and often `bash`; everything else is the script author's
/// responsibility to ensure.
fn parse_shebang_interpreter(body: &str) -> Option<&'static str> {
    let first_line = body.lines().next()?;
    let trimmed = first_line.strip_prefix("#!")?;
    if trimmed.contains("bash") {
        Some("bash")
    } else if trimmed.contains("zsh") {
        Some("zsh")
    } else if trimmed.contains("/sh") || trimmed.ends_with(" sh") || trimmed.trim_end() == "sh" {
        Some("sh")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use keel_container::{Backend, BackendError, ExecOptions, ServiceStatus};
    use std::sync::Mutex;

    #[test]
    fn shebang_bash() {
        assert_eq!(
            parse_shebang_interpreter("#!/usr/bin/env bash\necho hi\n"),
            Some("bash")
        );
        assert_eq!(parse_shebang_interpreter("#!/bin/bash\n"), Some("bash"));
    }

    #[test]
    fn shebang_zsh() {
        assert_eq!(
            parse_shebang_interpreter("#!/usr/bin/env zsh\n"),
            Some("zsh")
        );
    }

    #[test]
    fn shebang_sh() {
        assert_eq!(parse_shebang_interpreter("#!/bin/sh\n"), Some("sh"));
        assert_eq!(parse_shebang_interpreter("#!/usr/bin/env sh\n"), Some("sh"));
    }

    #[test]
    fn shebang_unknown() {
        assert_eq!(parse_shebang_interpreter("#!/usr/bin/python3\n"), None);
        assert_eq!(parse_shebang_interpreter("echo no shebang\n"), None);
        assert_eq!(parse_shebang_interpreter(""), None);
    }

    struct MockBackend {
        status: ServiceStatus,
        exec_log: Mutex<Vec<(String, Vec<String>)>>,
        /// Records (service, argv, stdin) for stdin-piped exec calls.
        exec_stdin_log: Mutex<Vec<(String, Vec<String>, String)>>,
        exec_code: i32,
    }

    impl MockBackend {
        fn new(status: ServiceStatus) -> Self {
            Self {
                status,
                exec_log: Mutex::new(Vec::new()),
                exec_stdin_log: Mutex::new(Vec::new()),
                exec_code: 0,
            }
        }
    }

    #[async_trait]
    impl Backend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }
        async fn status(&self, _service: &str) -> Result<ServiceStatus, BackendError> {
            Ok(self.status)
        }
        async fn exec(
            &self,
            service: &str,
            argv: &[&str],
            _opts: &ExecOptions,
        ) -> Result<i32, BackendError> {
            self.exec_log.lock().unwrap().push((
                service.to_string(),
                argv.iter().map(|s| (*s).to_string()).collect(),
            ));
            Ok(self.exec_code)
        }
        async fn passthrough(&self, _args: &[&str]) -> Result<i32, BackendError> {
            Ok(0)
        }
        async fn exec_with_stdin(
            &self,
            service: &str,
            argv: &[&str],
            _opts: &ExecOptions,
            stdin: &str,
        ) -> Result<i32, BackendError> {
            self.exec_stdin_log.lock().unwrap().push((
                service.to_string(),
                argv.iter().map(|s| (*s).to_string()).collect(),
                stdin.to_string(),
            ));
            Ok(self.exec_code)
        }
    }

    fn cfg(toml_src: &str) -> Arc<Config> {
        Arc::new(keel_config::parse_str(toml_src).unwrap())
    }

    #[tokio::test]
    async fn errors_on_unknown_recipe() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let exec = Executor::new(backend, cfg(""), Path::new("/tmp"));
        let err = exec.run_recipe("nope", &[]).await.unwrap_err();
        match err {
            RuntimeError::UnknownCommand { name, .. } => assert_eq!(name, "nope"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn errors_on_undefined_dependency() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.test]
            run = "true"
            needs = ["build"]
        "#);
        let exec = Executor::new(backend, cfg, Path::new("/tmp"));
        let err = exec.run_recipe("test", &[]).await.unwrap_err();
        match err {
            RuntimeError::UnknownDependency { recipe, dep } => {
                assert_eq!(recipe, "test");
                assert_eq!(dep, "build");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn errors_on_dependency_cycle() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.a]
            run = "true"
            needs = ["b"]

            [command.b]
            run = "true"
            needs = ["a"]
        "#);
        let exec = Executor::new(backend, cfg, Path::new("/tmp"));
        let err = exec.run_recipe("a", &[]).await.unwrap_err();
        assert!(matches!(err, RuntimeError::DependencyCycle(_)));
    }

    #[tokio::test]
    async fn refuses_to_exec_in_stopped_service() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Stopped));
        let cfg = cfg(r#"
            [command.shell]
            in = "app"
            run = "/bin/sh"
        "#);
        let exec = Executor::new(backend, cfg, Path::new("/tmp"));
        let err = exec.run_recipe("shell", &[]).await.unwrap_err();
        match err {
            RuntimeError::Backend(keel_container::BackendError::ServiceUnavailable {
                service,
                ..
            }) => assert_eq!(service, "app"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn execs_in_service_when_running() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.test]
            in = "app"
            run = "composer test"
            forward_args = true
        "#);
        let exec = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            cfg,
            Path::new("/tmp"),
        );
        let code = exec
            .run_recipe("test", &["--filter".into(), "Login".into()])
            .await
            .unwrap();
        assert_eq!(code, 0);
        let log = backend.exec_log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "app");
        assert_eq!(
            log[0].1,
            vec!["composer", "test", "--filter", "Login"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn sequential_array_forwards_args_to_last_step() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.first]
            in = "app"
            run = "echo first"

            [command.second]
            in = "app"
            run = "echo second"
            forward_args = true

            [command.combo]
            in = "app"
            run = ["first", "second"]
            forward_args = true
        "#);
        let exec = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            cfg,
            Path::new("/tmp"),
        );
        let code = exec.run_recipe("combo", &["arg".into()]).await.unwrap();
        assert_eq!(code, 0);
        let log = backend.exec_log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].1, vec!["echo".to_string(), "first".to_string()]);
        assert_eq!(
            log[1].1,
            vec!["echo".to_string(), "second".to_string(), "arg".to_string(),]
        );
    }

    #[tokio::test]
    async fn host_recipe_with_channel_sink_streams_output() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.echo]
            run = "sh -c 'echo hi from stdout; echo oops 1>&2'"
        "#);
        let project_root = std::env::current_dir().unwrap();
        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            cfg,
            project_root.as_path(),
        );

        let (sink, mut rx) = crate::sink::ChannelSink::new_pair();
        let exec = executor.with_sink(Arc::new(sink));
        let code = exec.run_recipe("echo", &[]).await.unwrap();
        assert_eq!(code, 0);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Ok(line) = rx.try_recv() {
            match line.stream {
                crate::sink::OutputStream::Stdout => stdout.push(line.line),
                crate::sink::OutputStream::Stderr => stderr.push(line.line),
            }
        }
        assert_eq!(stdout, vec!["hi from stdout"]);
        assert_eq!(stderr, vec!["oops"]);
    }

    #[tokio::test]
    async fn host_script_gets_keel_project_and_script_dirs() {
        // Build a host script that prints KEEL_PROJECT_DIR and
        // KEEL_SCRIPT_DIR to stdout. After running, the captured
        // output should contain both paths in the canonical form.
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let project_dir = tempfile::TempDir::new().unwrap();
        let scripts_subdir = project_dir.path().join(".keel/commands");
        std::fs::create_dir_all(&scripts_subdir).unwrap();
        let script_path = scripts_subdir.join("probe");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho KEEL_PROJECT_DIR=$KEEL_PROJECT_DIR\necho KEEL_SCRIPT_DIR=$KEEL_SCRIPT_DIR\n",
        )
        .unwrap();
        // Make it executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script_path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script_path, perm).unwrap();
        }

        let mut cfg_inner = keel_config::Config::default();
        cfg_inner.scripts.insert(
            "probe".into(),
            keel_config::ScriptCommand {
                name: "probe".into(),
                path: script_path.clone(),
                desc: None,
                service: None,
                tty: false,
                env: Default::default(),
                needs: Vec::new(),
                forward_args: false,
                cwd: None,
                optional: false,
                interactive: false,
            },
        );
        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            Arc::new(cfg_inner),
            project_dir.path(),
        );
        let (sink, mut rx) = crate::sink::ChannelSink::new_pair();
        let exec = executor.with_sink(Arc::new(sink));
        let code = exec.run_script("probe", &[]).await.unwrap();
        assert_eq!(code, 0);

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line.line);
        }
        let project_str = project_dir.path().display().to_string();
        let script_dir_str = scripts_subdir.display().to_string();
        assert!(
            lines
                .iter()
                .any(|l| l == &format!("KEEL_PROJECT_DIR={project_str}")),
            "lines: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == &format!("KEEL_SCRIPT_DIR={script_dir_str}")),
            "lines: {lines:?}"
        );
    }

    #[tokio::test]
    async fn script_env_overrides_keel_project_dir() {
        // A script that explicitly sets KEEL_PROJECT_DIR in its
        // `env = {...}` block should win over the default — sacred
        // vars stay overridable, no surprise lock-out.
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let project_dir = tempfile::TempDir::new().unwrap();
        let script_path = project_dir.path().join("probe");
        std::fs::write(&script_path, "#!/bin/sh\necho VAL=$KEEL_PROJECT_DIR\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script_path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script_path, perm).unwrap();
        }

        let mut cfg_inner = keel_config::Config::default();
        let mut env_overrides = std::collections::BTreeMap::new();
        env_overrides.insert("KEEL_PROJECT_DIR".into(), "OVERRIDDEN".into());
        cfg_inner.scripts.insert(
            "probe".into(),
            keel_config::ScriptCommand {
                name: "probe".into(),
                path: script_path.clone(),
                desc: None,
                service: None,
                tty: false,
                env: env_overrides,
                needs: Vec::new(),
                forward_args: false,
                cwd: None,
                optional: false,
                interactive: false,
            },
        );
        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            Arc::new(cfg_inner),
            project_dir.path(),
        );
        let (sink, mut rx) = crate::sink::ChannelSink::new_pair();
        let exec = executor.with_sink(Arc::new(sink));
        let code = exec.run_script("probe", &[]).await.unwrap();
        assert_eq!(code, 0);
        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line.line);
        }
        assert!(
            lines.iter().any(|l| l == "VAL=OVERRIDDEN"),
            "lines: {lines:?}"
        );
    }

    #[tokio::test]
    async fn in_container_script_pipes_body_to_interpreter() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(""); // No recipes; script will be injected manually.
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("setup");
        std::fs::write(
            &script_path,
            "#!/usr/bin/env bash\nset -euo pipefail\necho \"in container\"\n",
        )
        .unwrap();

        // Build a Config with the script attached. We can't use load_project here
        // (it scans .keel/commands/) so we synthesise a ScriptCommand directly.
        let mut cfg_inner = (*cfg).clone();
        cfg_inner.scripts.insert(
            "setup".into(),
            keel_config::ScriptCommand {
                name: "setup".into(),
                path: script_path.clone(),
                desc: None,
                service: Some("app".into()),
                tty: false,
                env: Default::default(),
                needs: Vec::new(),
                forward_args: true,
                cwd: None,
                optional: false,
                interactive: false,
            },
        );
        let cfg_arc = Arc::new(cfg_inner);

        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            cfg_arc,
            dir.path(),
        );
        let code = executor
            .run_script("setup", &["alpha".into(), "beta".into()])
            .await
            .unwrap();
        assert_eq!(code, 0);

        let log = backend.exec_stdin_log.lock().unwrap();
        assert_eq!(log.len(), 1);
        let (service, argv, stdin) = &log[0];
        assert_eq!(service, "app");
        // bash -s -- alpha beta
        assert_eq!(
            argv,
            &vec![
                "bash".to_string(),
                "-s".into(),
                "--".into(),
                "alpha".into(),
                "beta".into(),
            ]
        );
        // The piped body matches the script content verbatim.
        assert!(stdin.contains("set -euo pipefail"));
        assert!(stdin.contains("echo \"in container\""));
    }

    #[tokio::test]
    async fn in_container_script_refuses_when_service_stopped() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Stopped));
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("nope");
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        let mut cfg_inner = keel_config::Config::default();
        cfg_inner.scripts.insert(
            "nope".into(),
            keel_config::ScriptCommand {
                name: "nope".into(),
                path: script_path,
                desc: None,
                service: Some("app".into()),
                tty: false,
                env: Default::default(),
                needs: Vec::new(),
                forward_args: false,
                cwd: None,
                optional: false,
                interactive: false,
            },
        );
        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            Arc::new(cfg_inner),
            dir.path(),
        );
        let err = executor.run_script("nope", &[]).await.unwrap_err();
        match err {
            RuntimeError::Backend(keel_container::BackendError::ServiceUnavailable {
                service,
                ..
            }) => assert_eq!(service, "app"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parallel_array_runs_all_steps() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Running));
        let cfg = cfg(r#"
            [command.combo]
            in = "app"
            run = ["echo a", "echo b", "echo c"]
            parallel = true
        "#);
        let exec = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            cfg,
            Path::new("/tmp"),
        );
        let code = exec.run_recipe("combo", &[]).await.unwrap();
        assert_eq!(code, 0);
        let log = backend.exec_log.lock().unwrap();
        assert_eq!(log.len(), 3);
        // Order is non-deterministic under parallelism — just verify the set.
        let mut joined: Vec<String> = log.iter().map(|(_, argv)| argv.join(" ")).collect();
        joined.sort();
        assert_eq!(joined, vec!["echo a", "echo b", "echo c"]);
    }
}
