//! Recipe executor.
//!
//! Turns a recipe (already resolved by the [`Resolver`](crate::Resolver))
//! into actual process work, talking to a [`scaffl_container::Backend`] for
//! anything container-shaped. The CLI and the TUI both call into this; both
//! get the same semantics.
//!
//! Bounded responsibility: receive inputs, produce exit codes. Does not
//! decide *which* recipe to run — that's the resolver's job — and does not
//! parse anything from disk — that's the config's job.

use crate::env::Env;
use crate::error::RuntimeError;
use crate::sink::{InheritSink, OutputSink, OutputStream};
use scaffl_config::{Config, Recipe, Run, ScriptCommand};
use scaffl_container::{Backend, ExecOptions, ServiceStatus};
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
#[derive(Clone)]
pub struct Executor {
    backend: Arc<dyn Backend>,
    config: Arc<Config>,
    project_root: Arc<Path>,
    base_env: Arc<OnceCell<Env>>,
    sink: Arc<dyn OutputSink>,
    /// Active profile name, applied to recipes via [`Recipe::with_profile`].
    profile: Option<String>,
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
        }
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
                Env::resolve(&self.config, self.project_root.as_ref()).await
            })
            .await
    }

    /// Run a recipe by name. `args` are forwarded to the recipe's `run` if
    /// `forward_args = true`.
    pub async fn run_recipe(&self, name: &str, args: &[String]) -> Result<i32, RuntimeError> {
        self.run_recipe_inner(name.to_string(), args.to_vec(), HashSet::new())
            .await
    }

    /// Run a script command by name. Mirrors [`Self::run_recipe`] but
    /// dispatches to a `.scaffl/commands/<name>` file.
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
                    scaffl_container::BackendError::ServiceUnavailable {
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

        let env = self
            .base_env()
            .await?
            .clone()
            .with_overrides(recipe.env.iter().map(|(k, v)| (k.clone(), v.clone())));

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
                env: env.into_map(),
                workdir: None,
            };
            let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            return self
                .backend
                .exec(service, &argv_refs, &opts)
                .await
                .map_err(RuntimeError::from);
        }

        // Host execution.
        let (program, rest) = argv.split_first().expect("non-empty argv");
        let mut cmd = Command::new(program);
        cmd.args(rest);
        cmd.current_dir(self.project_root.as_ref());
        cmd.env_clear();
        for (k, v) in env.iter() {
            cmd.env(k, v);
        }
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
                .map_err(|e| RuntimeError::Backend(scaffl_container::BackendError::Spawn(e)))?;
            return Ok(status.code().unwrap_or(-1));
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| RuntimeError::Backend(scaffl_container::BackendError::Spawn(e)))?;
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
            .map_err(|e| RuntimeError::Backend(scaffl_container::BackendError::Spawn(e)))?;
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
        if let Some(service) = &script.service {
            // In-container script execution is deferred — staging the file
            // into the container and selecting an interpreter is not in
            // Phase 2's scope. Surface a clear error rather than silently
            // running on the host.
            return Err(RuntimeError::Backend(
                scaffl_container::BackendError::Reported(format!(
                    "script `{name}` declares `in = \"{service}\"`; in-container scripts are not yet supported (run on host or move the logic into a recipe with `in =`)",
                    name = script.name,
                )),
            ));
        }

        let env = self
            .base_env()
            .await?
            .clone()
            .with_overrides(script.env.iter().map(|(k, v)| (k.clone(), v.clone())));

        let mut cmd = Command::new(&script.path);
        if script.forward_args {
            cmd.args(args);
        }
        cmd.current_dir(self.project_root.as_ref());
        cmd.env_clear();
        for (k, v) in env.iter() {
            cmd.env(k, v);
        }
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
        let env = self.base_env().await?.clone();
        let opts = ExecOptions {
            tty,
            env: env.into_map(),
            workdir: None,
        };
        self.backend
            .exec(service, argv, &opts)
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use scaffl_container::{Backend, BackendError, ExecOptions, ServiceStatus};
    use std::sync::Mutex;

    struct MockBackend {
        status: ServiceStatus,
        exec_log: Mutex<Vec<(String, Vec<String>)>>,
        exec_code: i32,
    }

    impl MockBackend {
        fn new(status: ServiceStatus) -> Self {
            Self {
                status,
                exec_log: Mutex::new(Vec::new()),
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
    }

    fn cfg(toml_src: &str) -> Arc<Config> {
        Arc::new(scaffl_config::parse_str(toml_src).unwrap())
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
            RuntimeError::Backend(scaffl_container::BackendError::ServiceUnavailable {
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
