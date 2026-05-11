//! `keel doctor` — validate config and report.

use anyhow::Result;
use keel_config::Config;
use keel_container::{Backend, compose::ComposeBackend};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Fail,
}

impl Severity {
    fn marker(self) -> &'static str {
        match self {
            Severity::Ok => "[ OK ]",
            Severity::Warn => "[WARN]",
            Severity::Fail => "[FAIL]",
        }
    }
}

#[derive(Debug, Clone)]
struct Finding {
    severity: Severity,
    message: String,
}

impl Finding {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: message.into(),
        }
    }
    fn fail(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            message: message.into(),
        }
    }
}

/// Run all checks and print a report. Returns exit code 0 if no FAILs;
/// 1 otherwise. Warnings do not change the exit code.
pub async fn run(config: &Config, project_root: &Path) -> Result<i32> {
    let mut findings = Vec::new();
    findings.extend(check_backend(config).await);
    findings.extend(check_env_files(config, project_root));
    findings.extend(check_dependency_graph(config));
    findings.extend(check_service_hints(config));
    findings.extend(check_custom_services(config).await);
    findings.extend(check_worktree(config, project_root).await);
    findings.extend(check_devcontainer(config, project_root).await);

    let mut had_fail = false;
    for f in &findings {
        if f.severity == Severity::Fail {
            had_fail = true;
        }
        println!("{} {}", f.severity.marker(), f.message);
    }

    let summary = format!(
        "{} checks: {} ok, {} warn, {} fail",
        findings.len(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Ok)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Fail)
            .count(),
    );
    println!("\n{summary}");

    Ok(if had_fail { 1 } else { 0 })
}

async fn check_backend(config: &Config) -> Vec<Finding> {
    use keel_config::model::Backend as B;
    match config.runtime.backend {
        B::None => vec![Finding::ok(
            "backend: none (no container backend configured)",
        )],
        B::Compose => match ComposeBackend::detect().await {
            Ok(b) => vec![Finding::ok(format!("backend: {} detected", b.name()))],
            Err(_) => vec![Finding::fail(
                "backend: docker compose / docker-compose not found on PATH",
            )],
        },
        B::Docker | B::Podman => vec![Finding::warn(format!(
            "backend: `{:?}` is configured but only `compose` is implemented in this version",
            config.runtime.backend
        ))],
    }
}

fn check_env_files(config: &Config, project_root: &Path) -> Vec<Finding> {
    if config.env_files.files.is_empty() {
        return vec![Finding::ok("env_files: none declared")];
    }
    let mut out = Vec::new();
    for raw in &config.env_files.files {
        // We don't expand ${VAR} here — doctor is best-effort and a literal
        // path mismatch is still useful signal.
        let path = project_root.join(raw);
        if path.exists() {
            out.push(Finding::ok(format!("env file `{raw}` present")));
        } else {
            out.push(Finding::warn(format!(
                "env file `{raw}` not found (will be skipped at runtime)"
            )));
        }
    }
    out
}

fn check_dependency_graph(config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();
    let known: Vec<&String> = config
        .commands
        .keys()
        .chain(config.scripts.keys())
        .collect();

    let check_deps = |name: &str, deps: &[String], kind: &str| -> Vec<Finding> {
        let mut findings = Vec::new();
        for dep in deps {
            if !known.iter().any(|k| k.as_str() == dep) {
                findings.push(Finding::fail(format!(
                    "{kind} `{name}` depends on unknown `{dep}`"
                )));
            }
        }
        findings
    };

    for (name, recipe) in &config.commands {
        out.extend(check_deps(name, &recipe.needs, "recipe"));
    }
    for (name, script) in &config.scripts {
        out.extend(check_deps(name, &script.needs, "script"));
    }

    if out.is_empty() {
        out.push(Finding::ok(format!(
            "dependency graph: {} command(s), all `needs` resolve",
            known.len()
        )));
    }
    out
}

fn check_service_hints(config: &Config) -> Vec<Finding> {
    use keel_config::model::Backend as B;
    if !matches!(
        config.runtime.backend,
        B::Compose | B::Docker | B::Podman
    ) {
        return Vec::new();
    }
    // We can't read docker-compose.yaml here without a YAML parser. So we
    // just count how many recipes declare `in =` and report it as info.
    let with_service = config
        .commands
        .values()
        .filter(|r| r.service.is_some())
        .count()
        + config
            .scripts
            .values()
            .filter(|s| s.service.is_some())
            .count();
    if with_service == 0 {
        Vec::new()
    } else {
        vec![Finding::ok(format!(
            "{with_service} command(s) target a container service (verify with `compose ps`)"
        ))]
    }
}

/// Probe each declared custom / systemd service: run its status
/// command (or the systemd `is-active` equivalent) and report the
/// outcome. We don't fail the project on a stopped service — the
/// whole point of a status check is "tell me whether it's running"
/// — but a missing systemctl binary or unknown unit gets surfaced
/// as a warning so the user has a chance to fix the config.
async fn check_custom_services(config: &Config) -> Vec<Finding> {
    use keel_runtime::services::{from_custom, from_systemd};

    let mut findings = Vec::new();
    let mut entries = Vec::new();
    for svc in &config.services.custom {
        entries.push(("custom", from_custom(svc)));
    }
    for svc in &config.services.systemd {
        entries.push(("systemd", from_systemd(svc)));
    }
    if entries.is_empty() {
        return findings;
    }

    for (kind, entry) in &entries {
        match probe_status(&entry.status_cmd).await {
            ProbeResult::Running => {
                findings.push(Finding::ok(format!(
                    "{kind} service `{}`: running",
                    entry.name
                )));
            }
            ProbeResult::Stopped(code) => {
                findings.push(Finding::ok(format!(
                    "{kind} service `{}`: stopped (status exit {code})",
                    entry.name
                )));
            }
            ProbeResult::SpawnFailed(msg) => {
                findings.push(Finding::warn(format!(
                    "{kind} service `{}`: status command failed to spawn ({msg})",
                    entry.name
                )));
            }
        }
    }
    findings
}

enum ProbeResult {
    Running,
    Stopped(i32),
    SpawnFailed(String),
}

async fn probe_status(cmd: &str) -> ProbeResult {
    use std::process::Stdio;
    use tokio::process::Command;
    match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .await
    {
        Ok(s) if s.success() => ProbeResult::Running,
        Ok(s) => ProbeResult::Stopped(s.code().unwrap_or(-1)),
        Err(e) => ProbeResult::SpawnFailed(e.to_string()),
    }
}

async fn check_worktree(config: &Config, project_root: &Path) -> Vec<Finding> {
    let identity = keel_runtime::Identity::detect(project_root, config).await;
    if !identity.is_isolated() {
        return vec![Finding::ok(
            "worktree: no git branch detected (offset = 0, no compose isolation)",
        )];
    }
    let pinned = config.worktrees.assign.contains_key(&identity.slug);
    let source = if pinned { "pinned" } else { "hashed" };
    vec![Finding::ok(format!(
        "worktree: slug `{}` → offset {} ({source}); compose isolation {}",
        identity.slug,
        identity.offset,
        if config.worktrees.isolate_compose {
            "on"
        } else {
            "off"
        },
    ))]
}

/// Devcontainer health check. Only runs when the user has opted in;
/// silent (skipped from output entirely) when disabled, so a normal
/// project's doctor output isn't padded with "[OK] devcontainer:
/// disabled" noise.
async fn check_devcontainer(config: &Config, project_root: &Path) -> Vec<Finding> {
    use keel_container::devcontainer::{ContainerSource, DevcontainerSpec};

    if !config.devcontainer.enabled {
        return Vec::new();
    }

    let mut out = Vec::new();
    let path = match DevcontainerSpec::discover(
        project_root,
        config.devcontainer.path.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            out.push(Finding::fail(format!("devcontainer: {e}")));
            return out;
        }
    };
    out.push(Finding::ok(format!(
        "devcontainer: config at `{}`",
        path.display()
    )));

    let spec = match DevcontainerSpec::load(&path) {
        Ok(s) => s,
        Err(e) => {
            out.push(Finding::fail(format!("devcontainer: {e}")));
            return out;
        }
    };

    // Build source: validate dockerfile reachability so users don't
    // discover a typo on `ensure_up` (which would fail with a much
    // less specific error).
    if let ContainerSource::Build { dockerfile, .. } = &spec.source {
        if !dockerfile.is_file() {
            out.push(Finding::fail(format!(
                "devcontainer: `build.dockerfile` not found at `{}`",
                dockerfile.display()
            )));
        } else {
            out.push(Finding::ok(format!(
                "devcontainer: dockerfile `{}` exists",
                dockerfile.display()
            )));
        }
    }

    // Docker availability — without it the devcontainer is dead in
    // the water regardless of how good the spec is.
    if which::which("docker").is_err() {
        out.push(Finding::fail(
            "devcontainer: docker not found on PATH (install docker or set `[devcontainer] enabled = false`)",
        ));
        return out;
    }

    // Warn on privilege-escalating runArgs, mirroring the runtime
    // warning. Users may have opted in deliberately, but a quiet log
    // line at run time is easy to miss; doctor's job is to surface
    // the surprising.
    for arg in &spec.run_args {
        if arg == "--privileged"
            || arg == "--cap-add"
            || arg.starts_with("--cap-add=")
            || arg == "--network=host"
            || arg == "--net=host"
            || arg.starts_with("--network=host")
            || arg.starts_with("--net=host")
        {
            out.push(Finding::warn(format!(
                "devcontainer: runArgs includes `{arg}` (elevates privileges)"
            )));
        }
    }

    // Container state — informational, not pass/fail. `ensure_up`
    // will start a stopped one and create a missing one.
    let identity = keel_runtime::Identity::detect(project_root, config).await;
    let dc = match crate::app::build_devcontainer(config, project_root, &identity) {
        Ok(Some(b)) => b,
        // Build returned None only when `enabled` is false, already
        // handled above; Err is caught by the earlier discover/load
        // arms. Defensive return.
        Ok(None) | Err(_) => return out,
    };
    let plan = dc.plan();
    out.push(Finding::ok(format!(
        "devcontainer: container `{}` → image `{}` → workspace `{}`",
        plan.container_name, plan.image_ref, plan.workspace_folder
    )));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_config::parse_str;

    #[test]
    fn unknown_dep_is_fail() {
        let cfg = parse_str(
            r#"
            [command.test]
            run = "true"
            needs = ["build"]
        "#,
        )
        .unwrap();
        let findings = check_dependency_graph(&cfg);
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Fail && f.message.contains("build"))
        );
    }

    #[test]
    fn known_dep_passes() {
        let cfg = parse_str(
            r#"
            [command.build]
            run = "true"
            [command.test]
            run = "true"
            needs = ["build"]
        "#,
        )
        .unwrap();
        let findings = check_dependency_graph(&cfg);
        assert!(findings.iter().all(|f| f.severity != Severity::Fail));
    }

    #[test]
    fn missing_env_file_is_warn() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = parse_str(
            r#"
            [env_files]
            files = [".env.does-not-exist"]
        "#,
        )
        .unwrap();
        let findings = check_env_files(&cfg, dir.path());
        assert!(findings.iter().any(|f| f.severity == Severity::Warn));
    }
}
