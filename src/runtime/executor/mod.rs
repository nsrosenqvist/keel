//! Recipe executor.
//!
//! Turns a recipe (already resolved by the [`Resolver`](crate::runtime::Resolver))
//! into actual process work, talking to a [`crate::container::Backend`] for
//! anything container-shaped. The CLI and the TUI both call into this; both
//! get the same semantics.
//!
//! Bounded responsibility: receive inputs, produce exit codes. Does not
//! decide *which* recipe to run — that's the resolver's job — and does not
//! parse anything from disk — that's the config's job.
//!
//! Module layout (Phase 11 split):
//!  - `mod.rs` — `Executor` struct, builder methods, env caching, the
//!    pure-backend wrappers (`passthrough`, `service_exec`).
//!  - `recipe.rs` — recipe-run orchestration (`run_recipe`, dependency
//!    wiring, sequential / parallel step iteration).
//!  - `script.rs` — script-run orchestration mirroring recipe paths but
//!    with stdin-piped script bodies.
//!  - `step.rs` — per-step dispatch: decides between service exec,
//!    devcontainer routing, and host spawn.
//!  - `host.rs` — host-process spawning + child-stream pumping shared
//!    with the in-container stdin-pipe path.

use crate::config::Config;
use crate::container::devcontainer::DevcontainerBackend;
use crate::container::{Backend, ExecOptions};
use crate::runtime::env::Env;
use crate::runtime::error::RuntimeError;
use crate::runtime::sink::{InheritSink, OutputSink};
use crate::runtime::worktree::Identity;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::OnceCell;

mod host;
mod recipe;
mod script;
mod step;

pub(crate) type BoxFut<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

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

/// The executor.
///
/// Holds shared, immutable references to its collaborators. Cheap to clone
/// for spawning concurrent recipe steps; the cached base env is shared via
/// [`Arc`].
#[derive(Clone)]
pub struct Executor {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) config: Arc<Config>,
    pub(crate) project_root: Arc<Path>,
    pub(crate) base_env: Arc<OnceCell<Env>>,
    pub(crate) sink: Arc<dyn OutputSink>,
    /// Active profile name, applied to recipes via [`crate::config::Recipe::with_profile`].
    pub(crate) profile: Option<String>,
    /// Pre-detected worktree identity. When `None`, [`Env::resolve`]
    /// auto-detects on first use. CLI / TUI pass a known identity to
    /// avoid duplicate `git rev-parse` invocations.
    pub(crate) identity: Option<Identity>,
    /// Where no-`in` work lands. Defaults to [`WorkspaceTarget::Local`].
    pub(crate) workspace: WorkspaceTarget,
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
    pub(crate) async fn cached_base_env(&self) -> Result<&Env, RuntimeError> {
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
    pub(crate) async fn effective_env(
        &self,
        overrides: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<Env, RuntimeError> {
        let env = self.cached_base_env().await?.clone();
        match overrides {
            Some(o) => Ok(env.with_overrides(o.iter().map(|(k, v)| (k.as_str(), v.as_str())))),
            None => Ok(env),
        }
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

    /// Dispatch a hook-style invocation: a fully-resolved argv plus
    /// an optional service routing. Centralises the three spawn
    /// surfaces (service exec / devcontainer exec / host) so the hook
    /// runner doesn't have to duplicate the dispatch logic.
    ///
    /// - `service = Some(svc)` → exec via the configured backend.
    /// - `service = None`, workspace target =
    ///   [`WorkspaceTarget::Devcontainer`] → exec inside the devcontainer.
    /// - `service = None`, workspace target = `Local` → host spawn with
    ///   `current_dir = host_cwd`.
    pub async fn hook_exec(
        &self,
        service: Option<&str>,
        argv: &[&str],
        host_cwd: &Path,
    ) -> Result<i32, RuntimeError> {
        if argv.is_empty() {
            return Err(RuntimeError::ArgvParse {
                input: String::new(),
                message: "empty argv".into(),
            });
        }
        let env = self.effective_env(None).await?;

        if let Some(svc) = service {
            let opts = ExecOptions {
                tty: false,
                env: env.project_only_map(),
                workdir: None,
            };
            return self
                .backend
                .exec(svc, argv, &opts)
                .await
                .map_err(Into::into);
        }

        if let WorkspaceTarget::Devcontainer(dc) = &self.workspace {
            let opts = ExecOptions {
                tty: false,
                env: env.project_only_map(),
                workdir: None,
            };
            return dc
                .exec(dc.container_name(), argv, &opts)
                .await
                .map_err(Into::into);
        }

        let (program, rest) = argv.split_first().expect("argv non-empty above");
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(rest.iter().copied());
        cmd.current_dir(host_cwd);
        env.apply_to(&mut cmd);
        self.spawn_host(cmd).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{Backend, BackendError, ExecOptions, ServiceStatus};
    use async_trait::async_trait;
    use std::sync::Mutex;

    pub(super) struct MockBackend {
        pub status: ServiceStatus,
        pub exec_log: Mutex<Vec<(String, Vec<String>)>>,
        /// Records (service, argv, stdin) for stdin-piped exec calls.
        pub exec_stdin_log: Mutex<Vec<(String, Vec<String>, String)>>,
        pub exec_code: i32,
    }

    impl MockBackend {
        pub fn new(status: ServiceStatus) -> Self {
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

    pub(super) fn cfg(toml_src: &str) -> Arc<Config> {
        Arc::new(crate::config::parse_str(toml_src).unwrap())
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
            RuntimeError::Backend(crate::container::BackendError::ServiceUnavailable {
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

        let (sink, mut rx) = crate::runtime::sink::ChannelSink::new_pair();
        let exec = executor.with_sink(Arc::new(sink));
        let code = exec.run_recipe("echo", &[]).await.unwrap();
        assert_eq!(code, 0);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Ok(line) = rx.try_recv() {
            match line.stream {
                crate::runtime::sink::OutputStream::Stdout => stdout.push(line.line),
                crate::runtime::sink::OutputStream::Stderr => stderr.push(line.line),
            }
        }
        assert_eq!(stdout, vec!["hi from stdout"]);
        assert_eq!(stderr, vec!["oops"]);
    }

    #[tokio::test]
    async fn host_script_gets_keel_project_and_script_dirs() {
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script_path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script_path, perm).unwrap();
        }

        let mut cfg_inner = crate::config::Config::default();
        cfg_inner.scripts.insert(
            "probe".into(),
            crate::config::ScriptCommand {
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
        let (sink, mut rx) = crate::runtime::sink::ChannelSink::new_pair();
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

        let mut cfg_inner = crate::config::Config::default();
        let mut env_overrides = std::collections::BTreeMap::new();
        env_overrides.insert("KEEL_PROJECT_DIR".into(), "OVERRIDDEN".into());
        cfg_inner.scripts.insert(
            "probe".into(),
            crate::config::ScriptCommand {
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
        let (sink, mut rx) = crate::runtime::sink::ChannelSink::new_pair();
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
        let cfg = cfg("");
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("setup");
        std::fs::write(
            &script_path,
            "#!/usr/bin/env bash\nset -euo pipefail\necho \"in container\"\n",
        )
        .unwrap();

        let mut cfg_inner = (*cfg).clone();
        cfg_inner.scripts.insert(
            "setup".into(),
            crate::config::ScriptCommand {
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
        assert!(stdin.contains("set -euo pipefail"));
        assert!(stdin.contains("echo \"in container\""));
    }

    #[tokio::test]
    async fn in_container_script_refuses_when_service_stopped() {
        let backend = Arc::new(MockBackend::new(ServiceStatus::Stopped));
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("nope");
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        let mut cfg_inner = crate::config::Config::default();
        cfg_inner.scripts.insert(
            "nope".into(),
            crate::config::ScriptCommand {
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
            RuntimeError::Backend(crate::container::BackendError::ServiceUnavailable {
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
        let mut joined: Vec<String> = log.iter().map(|(_, argv)| argv.join(" ")).collect();
        joined.sort();
        assert_eq!(joined, vec!["echo a", "echo b", "echo c"]);
    }
}
