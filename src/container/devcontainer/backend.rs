//! `DevcontainerBackend` — drives `docker` to materialise the workspace
//! container described by a [`DevcontainerSpec`] and run commands
//! inside it.
//!
//! Lifecycle (called via [`ensure_up`]):
//! 1. Resolve the image: pre-built (`image: "..."`) is used as-is;
//!    `build: { dockerfile, ... }` is built and tagged with a hash of
//!    the dockerfile + build args, so rebuilds happen only when the
//!    inputs change.
//! 2. Inspect the deterministic container name. If missing, `docker
//!    run -d` it (mounting the workspace, applying `containerEnv`,
//!    `--label`s, `runArgs`). If stopped, `docker start`.
//!
//! Containers are kept alive with `sleep infinity` — the spec's
//! standard "no entrypoint" recipe. Per-recipe / per-shell work
//! happens through `docker exec`.
//!
//! [`ensure_up`]: DevcontainerBackend::ensure_up

use crate::container::devcontainer::config::{ContainerSource, DevcontainerSpec};
use crate::container::{Backend, BackendError, ExecOptions, ServiceStatus};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Stdio;
use thiserror::Error;
use tokio::process::{Child, Command};
use tracing::{debug, warn};

/// Identity inputs for container-name derivation. Bundled so the
/// caller can compute slugs once (typically via
/// `crate::runtime::worktree::Identity`) and hand them in.
#[derive(Debug, Clone)]
pub struct DevcontainerIdentity {
    /// Absolute path to the project (or worktree) root. Becomes the
    /// bind-mount source and the `keel.devcontainer.root` label
    /// value.
    pub project_root: PathBuf,
    /// Slug derived from the project name (typically root basename
    /// passed through `slugify`).
    pub project_slug: String,
    /// Slug derived from the active worktree. Empty when the project
    /// isn't a linked worktree (`Identity::is_isolated` == false).
    pub worktree_slug: String,
}

#[derive(Debug, Error)]
pub enum DevcontainerError {
    #[error("docker not found on PATH (install docker or use `runtime.backend = \"none\"`)")]
    DockerMissing,

    #[error("docker invocation failed: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("docker {step} exited {code}: {stderr}")]
    DockerFailed {
        step: &'static str,
        code: i32,
        stderr: String,
    },

    #[error("dockerfile not found: {0}")]
    DockerfileMissing(PathBuf),

    #[error("failed to read dockerfile {path}: {source}")]
    DockerfileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What [`ensure_up`] will do. Exposed for diagnostics (`keel
/// doctor`, dry-run output) — the actual work happens in
/// [`ensure_up`] which calls into the same plan internally.
///
/// [`ensure_up`]: DevcontainerBackend::ensure_up
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsurePlan {
    pub container_name: String,
    pub image_ref: String,
    pub workspace_folder: String,
    pub needs_build: bool,
}

#[derive(Debug)]
pub struct DevcontainerBackend {
    spec: DevcontainerSpec,
    identity: DevcontainerIdentity,
    container_name: String,
    workspace_folder: String,
    image_ref: String,
}

impl DevcontainerBackend {
    /// Build a backend bound to a spec + identity. Pure: no docker
    /// calls happen until [`ensure_up`] or an `exec` is invoked.
    ///
    /// [`ensure_up`]: Self::ensure_up
    pub fn new(spec: DevcontainerSpec, identity: DevcontainerIdentity) -> Self {
        let container_name = derive_container_name(&identity);
        let workspace_folder = resolve_workspace_folder(&spec, &identity);
        let image_ref = derive_image_ref(&spec, &identity);
        Self {
            spec,
            identity,
            container_name,
            workspace_folder,
            image_ref,
        }
    }

    pub fn spec(&self) -> &DevcontainerSpec {
        &self.spec
    }

    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    pub fn workspace_folder(&self) -> &str {
        &self.workspace_folder
    }

    pub fn image_ref(&self) -> &str {
        &self.image_ref
    }

    /// What [`ensure_up`] would do without doing it. Cheap (no docker
    /// calls); used by `keel doctor` to summarise state.
    ///
    /// [`ensure_up`]: Self::ensure_up
    pub fn plan(&self) -> EnsurePlan {
        EnsurePlan {
            container_name: self.container_name.clone(),
            image_ref: self.image_ref.clone(),
            workspace_folder: self.workspace_folder.clone(),
            needs_build: matches!(self.spec.source, ContainerSource::Build { .. }),
        }
    }

    /// Idempotent "container is running and ready for exec". Builds
    /// the image if needed, starts the container if stopped, runs it
    /// if missing.
    ///
    /// On the happy path this is a single `docker inspect` (cached
    /// state lookup); only first-time provisioning pays the build/run
    /// cost.
    pub async fn ensure_up(&self) -> Result<(), DevcontainerError> {
        ensure_docker_available()?;

        if let ContainerSource::Build {
            dockerfile,
            context,
            args,
        } = &self.spec.source
        {
            if !dockerfile.is_file() {
                return Err(DevcontainerError::DockerfileMissing(dockerfile.clone()));
            }
            if !image_exists(&self.image_ref).await? {
                debug!(image = %self.image_ref, "building devcontainer image");
                build_image(&self.image_ref, dockerfile, context, args).await?;
            }
        }

        match container_state(&self.container_name).await? {
            ContainerState::Running => Ok(()),
            ContainerState::Stopped => {
                debug!(name = %self.container_name, "starting stopped devcontainer");
                start_container(&self.container_name).await
            }
            ContainerState::Missing => {
                debug!(name = %self.container_name, "creating devcontainer");
                self.run_container().await
            }
        }
    }

    async fn run_container(&self) -> Result<(), DevcontainerError> {
        let mut cmd = Command::new("docker");
        cmd.args(["run", "-d", "--name", &self.container_name]);

        // Labels for lookup + clean-up. Keep these synced with the
        // doctor / shell command's lookup path.
        cmd.args([
            "--label",
            &format!(
                "keel.devcontainer.root={}",
                self.identity.project_root.display()
            ),
        ]);
        if !self.identity.worktree_slug.is_empty() {
            cmd.args([
                "--label",
                &format!("keel.devcontainer.worktree={}", self.identity.worktree_slug),
            ]);
        }

        // Bind-mount the workspace.
        cmd.args([
            "-v",
            &format!(
                "{}:{}",
                self.identity.project_root.display(),
                self.workspace_folder
            ),
        ]);
        cmd.args(["-w", &self.workspace_folder]);

        // containerEnv — baked in at run time; remoteEnv is exec-time.
        for (k, v) in &self.spec.container_env {
            cmd.arg("-e").arg(format!("{k}={v}"));
        }

        if let Some(user) = &self.spec.remote_user {
            cmd.args(["--user", user]);
        }

        // Pass-through runArgs with a privilege-escalation warning.
        // The warning matches the doctor check so users can't be
        // surprised by silent elevation.
        for arg in &self.spec.run_args {
            if is_privilege_escalating(arg) {
                warn!(
                    arg = %arg,
                    "devcontainer.json runArgs includes a privilege-escalating flag",
                );
            }
            cmd.arg(arg);
        }

        cmd.arg(&self.image_ref);

        // Standard devcontainer keep-alive: a foreground sleep so the
        // container has a PID 1 that won't exit. `sleep infinity` is
        // not portable to BusyBox; use `sh -c` with a portable form.
        cmd.args(["sh", "-c", "while true; do sleep 3600; done"]);

        run_docker(&mut cmd, "run").await
    }

    /// Build the `docker exec` command for a single invocation.
    /// `pipe_stdin = true` forces `-i` (no TTY) regardless of
    /// `opts.tty`, because `docker exec -it` rejects piped stdin.
    fn build_exec_command(&self, argv: &[&str], opts: &ExecOptions, pipe_stdin: bool) -> Command {
        let effective_opts;
        let opts_ref = if pipe_stdin && opts.tty {
            effective_opts = ExecOptions {
                tty: false,
                env: opts.env.clone(),
                workdir: opts.workdir.clone(),
            };
            &effective_opts
        } else {
            opts
        };
        let built = self.exec_argv(argv, opts_ref);
        let (head, tail) = built
            .split_first()
            .expect("exec_argv always produces at least program + subcommand");
        let mut cmd = Command::new(head);
        cmd.args(tail);
        if pipe_stdin {
            cmd.stdin(Stdio::piped());
        }
        cmd
    }

    /// Argv builder for `docker exec`. Split out so tests can snapshot
    /// the argv shape without invoking docker.
    pub fn exec_argv(&self, argv: &[&str], opts: &ExecOptions) -> Vec<String> {
        let mut out: Vec<String> = vec!["docker".into(), "exec".into()];
        if opts.tty {
            out.push("-it".into());
        } else {
            out.push("-i".into());
        }
        // remoteEnv first, recipe env last — recipe wins on conflict.
        for (k, v) in &self.spec.remote_env {
            out.push("-e".into());
            out.push(format!("{k}={v}"));
        }
        for (k, v) in &opts.env {
            out.push("-e".into());
            out.push(format!("{k}={v}"));
        }
        let workdir = opts
            .workdir
            .clone()
            .unwrap_or_else(|| self.workspace_folder.clone());
        out.push("-w".into());
        out.push(workdir);
        if let Some(user) = &self.spec.remote_user {
            out.push("--user".into());
            out.push(user.clone());
        }
        out.push(self.container_name.clone());
        for a in argv {
            out.push((*a).to_string());
        }
        out
    }
}

/// Container name pattern: `keel-devcontainer-<project-slug>[-<worktree-slug>]`.
/// Docker permits `[a-zA-Z0-9][a-zA-Z0-9_.-]*` so the slug rules from
/// `keel-runtime::worktree::slugify` (lowercase alnum + `-`) compose
/// cleanly.
fn derive_container_name(identity: &DevcontainerIdentity) -> String {
    let project = if identity.project_slug.is_empty() {
        "project".to_string()
    } else {
        identity.project_slug.clone()
    };
    if identity.worktree_slug.is_empty() {
        format!("keel-devcontainer-{project}")
    } else {
        format!("keel-devcontainer-{project}-{}", identity.worktree_slug)
    }
}

/// Resolve the workspaceFolder, applying the spec default
/// (`/workspaces/<repo-name>`) when the spec doesn't set one. The
/// project root's basename stands in for the repo name.
fn resolve_workspace_folder(spec: &DevcontainerSpec, identity: &DevcontainerIdentity) -> String {
    if let Some(folder) = &spec.workspace_folder {
        return folder.clone();
    }
    let basename = identity
        .project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace");
    format!("/workspaces/{basename}")
}

/// Image reference: the literal `image` for pulled images, or
/// `keel-devcontainer-<slug>:<sha256(dockerfile+args)[..12]>` for
/// `build` mode. The hash makes rebuilds happen only when the
/// dockerfile or build args change (rough approximation — context
/// changes aren't covered, intentionally; users who edit context
/// files can `docker rm` to force a rebuild).
fn derive_image_ref(spec: &DevcontainerSpec, identity: &DevcontainerIdentity) -> String {
    match &spec.source {
        ContainerSource::Image(image) => image.clone(),
        ContainerSource::Build {
            dockerfile, args, ..
        } => {
            let mut hasher = Sha256::new();
            if let Ok(bytes) = std::fs::read(dockerfile) {
                hasher.update(&bytes);
            } else {
                // Dockerfile may not exist yet at construction time
                // (lazy `ensure_up`). Hash the path so the tag is at
                // least deterministic; `ensure_up` will surface the
                // missing-file error properly.
                hasher.update(dockerfile.display().to_string().as_bytes());
            }
            for (k, v) in args {
                hasher.update(b"\0");
                hasher.update(k.as_bytes());
                hasher.update(b"=");
                hasher.update(v.as_bytes());
            }
            let digest = hex::encode(hasher.finalize());
            let tag: String = digest.chars().take(12).collect();
            let project = if identity.project_slug.is_empty() {
                "project".to_string()
            } else {
                identity.project_slug.clone()
            };
            format!("keel-devcontainer-{project}:{tag}")
        }
    }
}

fn ensure_docker_available() -> Result<(), DevcontainerError> {
    if which::which("docker").is_err() {
        return Err(DevcontainerError::DockerMissing);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerState {
    Running,
    Stopped,
    Missing,
}

async fn container_state(name: &str) -> Result<ContainerState, DevcontainerError> {
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Status}}", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !output.status.success() {
        return Ok(ContainerState::Missing);
    }
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(match status.as_str() {
        "running" => ContainerState::Running,
        "" | "missing" => ContainerState::Missing,
        _ => ContainerState::Stopped,
    })
}

async fn image_exists(reference: &str) -> Result<bool, DevcontainerError> {
    let output = Command::new("docker")
        .args(["image", "inspect", reference])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;
    Ok(output.status.success())
}

async fn build_image(
    tag: &str,
    dockerfile: &PathBuf,
    context: &PathBuf,
    args: &std::collections::BTreeMap<String, String>,
) -> Result<(), DevcontainerError> {
    let mut cmd = Command::new("docker");
    cmd.args(["build", "-t", tag, "-f"]);
    cmd.arg(dockerfile);
    for (k, v) in args {
        cmd.arg("--build-arg").arg(format!("{k}={v}"));
    }
    cmd.arg(context);
    run_docker(&mut cmd, "build").await
}

async fn start_container(name: &str) -> Result<(), DevcontainerError> {
    let mut cmd = Command::new("docker");
    cmd.args(["start", name]);
    run_docker(&mut cmd, "start").await
}

async fn run_docker(cmd: &mut Command, step: &'static str) -> Result<(), DevcontainerError> {
    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !output.status.success() {
        return Err(DevcontainerError::DockerFailed {
            step,
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

fn is_privilege_escalating(arg: &str) -> bool {
    matches!(
        arg,
        "--privileged" | "--cap-add" | "--network=host" | "--net=host"
    ) || arg.starts_with("--cap-add=")
        || arg.starts_with("--network=host")
        || arg.starts_with("--net=host")
}

#[async_trait]
impl Backend for DevcontainerBackend {
    fn name(&self) -> &'static str {
        "devcontainer"
    }

    async fn status(&self, _service: &str) -> Result<ServiceStatus, BackendError> {
        ensure_docker_available().map_err(|e| BackendError::Reported(e.to_string()))?;
        let state = container_state(&self.container_name)
            .await
            .map_err(|e| BackendError::Reported(e.to_string()))?;
        Ok(match state {
            ContainerState::Running => ServiceStatus::Running,
            ContainerState::Stopped => ServiceStatus::Stopped,
            ContainerState::Missing => ServiceStatus::Missing,
        })
    }

    async fn exec(
        &self,
        _service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<i32, BackendError> {
        self.ensure_up()
            .await
            .map_err(|e| BackendError::Reported(e.to_string()))?;
        let mut cmd = self.build_exec_command(argv, opts, /*pipe_stdin=*/ false);
        let status = cmd.status().await.map_err(BackendError::Spawn)?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn exec_with_stdin(
        &self,
        _service: &str,
        argv: &[&str],
        opts: &ExecOptions,
        stdin: &str,
    ) -> Result<i32, BackendError> {
        use tokio::io::AsyncWriteExt;

        self.ensure_up()
            .await
            .map_err(|e| BackendError::Reported(e.to_string()))?;
        let mut cmd = self.build_exec_command(argv, opts, /*pipe_stdin=*/ true);
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(BackendError::Spawn)?;
        if let Some(mut stdin_handle) = child.stdin.take() {
            stdin_handle
                .write_all(stdin.as_bytes())
                .await
                .map_err(BackendError::Spawn)?;
            drop(stdin_handle);
        }
        let status = child.wait().await.map_err(BackendError::Spawn)?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn spawn_exec(
        &self,
        _service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<Child, BackendError> {
        self.ensure_up()
            .await
            .map_err(|e| BackendError::Reported(e.to_string()))?;
        let mut cmd = self.build_exec_command(argv, opts, /*pipe_stdin=*/ false);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }

    async fn spawn_exec_with_stdin(
        &self,
        _service: &str,
        argv: &[&str],
        opts: &ExecOptions,
    ) -> Result<Child, BackendError> {
        self.ensure_up()
            .await
            .map_err(|e| BackendError::Reported(e.to_string()))?;
        let mut cmd = self.build_exec_command(argv, opts, /*pipe_stdin=*/ true);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }

    async fn passthrough(&self, args: &[&str]) -> Result<i32, BackendError> {
        let status = Command::new("docker")
            .args(args)
            .status()
            .await
            .map_err(BackendError::Spawn)?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn list_services(&self) -> Result<Vec<String>, BackendError> {
        Ok(vec![self.container_name.clone()])
    }

    async fn service_action(
        &self,
        action: &str,
        _services: &[&str],
    ) -> Result<Child, BackendError> {
        let mut cmd = Command::new("docker");
        match action {
            "up" => {
                // ensure_up isn't a long-running process; emulate one
                // by piping its outcome through `docker logs` so the
                // TUI's service-action stream gets *something* to
                // surface. Simpler: run the ensure synchronously and
                // hand back a `docker container ls` watcher.
                self.ensure_up()
                    .await
                    .map_err(|e| BackendError::Reported(e.to_string()))?;
                cmd.args(["logs", "-f", "--tail", "1", &self.container_name]);
            }
            "down" => {
                cmd.args(["rm", "-f", &self.container_name]);
            }
            "stop" => {
                cmd.args(["stop", &self.container_name]);
            }
            "restart" => {
                cmd.args(["restart", &self.container_name]);
            }
            other => {
                return Err(BackendError::Reported(format!(
                    "unsupported service action `{other}` (expected up / down / stop / restart)"
                )));
            }
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }

    async fn tail_logs(&self, _service: &str) -> Result<Child, BackendError> {
        let mut cmd = Command::new("docker");
        cmd.args(["logs", "-f", "--tail", "200", &self.container_name]);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn spec_image() -> DevcontainerSpec {
        DevcontainerSpec::from_str(
            Path::new("/proj/.devcontainer/devcontainer.json"),
            r#"{ "image": "alpine:latest" }"#,
        )
        .unwrap()
    }

    fn ident(slug: &str, worktree: &str) -> DevcontainerIdentity {
        DevcontainerIdentity {
            project_root: PathBuf::from("/home/me/proj"),
            project_slug: slug.into(),
            worktree_slug: worktree.into(),
        }
    }

    #[test]
    fn container_name_includes_worktree_slug_when_isolated() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", "feature-x"));
        assert_eq!(backend.container_name(), "keel-devcontainer-keel-feature-x");
    }

    #[test]
    fn container_name_omits_worktree_when_empty() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        assert_eq!(backend.container_name(), "keel-devcontainer-keel");
    }

    #[test]
    fn container_name_falls_back_when_project_slug_empty() {
        let backend = DevcontainerBackend::new(spec_image(), ident("", ""));
        assert_eq!(backend.container_name(), "keel-devcontainer-project");
    }

    #[test]
    fn workspace_folder_uses_spec_value_when_present() {
        let spec = DevcontainerSpec::from_str(
            Path::new("/proj/devcontainer.json"),
            r#"{ "image": "alpine", "workspaceFolder": "/code" }"#,
        )
        .unwrap();
        let backend = DevcontainerBackend::new(spec, ident("keel", ""));
        assert_eq!(backend.workspace_folder(), "/code");
    }

    #[test]
    fn workspace_folder_defaults_to_workspaces_basename() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        assert_eq!(backend.workspace_folder(), "/workspaces/proj");
    }

    #[test]
    fn image_ref_passes_through_image_field() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        assert_eq!(backend.image_ref(), "alpine:latest");
    }

    #[test]
    fn image_ref_hashes_when_building() {
        let spec = DevcontainerSpec::from_str(
            Path::new("/no/such/devcontainer.json"),
            r#"{ "build": { "dockerfile": "Dockerfile" } }"#,
        )
        .unwrap();
        let backend = DevcontainerBackend::new(spec, ident("keel", ""));
        // No filesystem read — hash falls back to path bytes. Just
        // assert the *shape* is right.
        let image_ref = backend.image_ref();
        assert!(
            image_ref.starts_with("keel-devcontainer-keel:"),
            "got: {image_ref}"
        );
        let tag = image_ref
            .split_once(':')
            .map(|(_, t)| t)
            .unwrap_or_default();
        assert_eq!(tag.len(), 12);
    }

    #[test]
    fn exec_argv_uses_tty_flag_when_requested() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        let argv = backend.exec_argv(
            &["sh", "-c", "echo hi"],
            &ExecOptions {
                tty: true,
                ..Default::default()
            },
        );
        assert_eq!(argv[0], "docker");
        assert_eq!(argv[1], "exec");
        assert_eq!(argv[2], "-it");
        assert!(argv.contains(&"sh".to_string()));
        assert!(argv.contains(&"-c".to_string()));
    }

    #[test]
    fn exec_argv_uses_dash_i_when_non_tty() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        let argv = backend.exec_argv(&["true"], &ExecOptions::default());
        assert_eq!(argv[2], "-i");
    }

    #[test]
    fn exec_argv_includes_remote_env_then_recipe_env() {
        let spec = DevcontainerSpec::from_str(
            Path::new("/proj/devcontainer.json"),
            r#"{ "image": "alpine", "remoteEnv": { "EDITOR": "vim" } }"#,
        )
        .unwrap();
        let backend = DevcontainerBackend::new(spec, ident("keel", ""));
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let argv = backend.exec_argv(
            &["true"],
            &ExecOptions {
                env,
                ..Default::default()
            },
        );
        // remoteEnv appears, then recipe env. Each -e is followed by KEY=VAL.
        let env_positions: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter_map(|(i, s)| (s == "-e").then_some(i))
            .collect();
        assert_eq!(env_positions.len(), 2);
        assert_eq!(argv[env_positions[0] + 1], "EDITOR=vim");
        assert_eq!(argv[env_positions[1] + 1], "FOO=bar");
    }

    #[test]
    fn exec_argv_workdir_defaults_to_workspace_folder() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        let argv = backend.exec_argv(&["true"], &ExecOptions::default());
        let w_idx = argv.iter().position(|s| s == "-w").expect("has -w");
        assert_eq!(argv[w_idx + 1], "/workspaces/proj");
    }

    #[test]
    fn exec_argv_workdir_uses_opts_override() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        let argv = backend.exec_argv(
            &["true"],
            &ExecOptions {
                workdir: Some("/elsewhere".into()),
                ..Default::default()
            },
        );
        let w_idx = argv.iter().position(|s| s == "-w").expect("has -w");
        assert_eq!(argv[w_idx + 1], "/elsewhere");
    }

    #[test]
    fn exec_argv_passes_remote_user() {
        let spec = DevcontainerSpec::from_str(
            Path::new("/proj/devcontainer.json"),
            r#"{ "image": "alpine", "remoteUser": "vscode" }"#,
        )
        .unwrap();
        let backend = DevcontainerBackend::new(spec, ident("keel", ""));
        let argv = backend.exec_argv(&["true"], &ExecOptions::default());
        let u_idx = argv.iter().position(|s| s == "--user").expect("has --user");
        assert_eq!(argv[u_idx + 1], "vscode");
    }

    #[test]
    fn privilege_escalating_detection() {
        assert!(is_privilege_escalating("--privileged"));
        assert!(is_privilege_escalating("--cap-add=SYS_PTRACE"));
        assert!(is_privilege_escalating("--network=host"));
        assert!(is_privilege_escalating("--net=host"));
        assert!(!is_privilege_escalating("--init"));
        assert!(!is_privilege_escalating("--cap-drop=ALL"));
    }

    #[test]
    fn plan_reflects_build_source() {
        let spec = DevcontainerSpec::from_str(
            Path::new("/proj/devcontainer.json"),
            r#"{ "build": { "dockerfile": "Dockerfile" } }"#,
        )
        .unwrap();
        let backend = DevcontainerBackend::new(spec, ident("keel", ""));
        let plan = backend.plan();
        assert!(plan.needs_build);
        assert_eq!(plan.container_name, "keel-devcontainer-keel");
    }

    #[test]
    fn plan_reflects_image_source() {
        let backend = DevcontainerBackend::new(spec_image(), ident("keel", ""));
        let plan = backend.plan();
        assert!(!plan.needs_build);
        assert_eq!(plan.image_ref, "alpine:latest");
    }
}
