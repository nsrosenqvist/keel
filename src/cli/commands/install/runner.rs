//! Drive the install plan: spawn each step, stream output into the
//! renderer, update state, decide success/failure.
//!
//! The runner is the only piece that owns the `Renderer` mutably, so
//! step execution always returns to it between steps — no shared
//! mutex, no thread-juggling. The line-streaming pattern is a
//! `tokio::select!` between three sources:
//!
//! - the child process's exit (resolves the loop),
//! - line-by-line output from a pair of stdio reader tasks,
//! - a periodic ticker that advances the spinner.

use crate::cli::commands::install::plan::{Step, StepSource, resolve_cwd};
use crate::cli::commands::install::renderer::{Renderer, StepOutcome};
use crate::cli::commands::install::state::{InstallState, StepStatus, epoch_ms};
use crate::config::{Config, InlineStep, InstallStepScript};
use crate::container::Backend;
use crate::runtime::{ChannelSink, Env, Executor};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

const TICK_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct InstallArgs {
    /// Single-step mode: run only this step. State is updated for the
    /// named step but other steps are not consulted or modified.
    pub step: Option<String>,
    /// `--resume`: non-interactive resume from the first non-resolved
    /// step. Errors when state file is absent.
    pub resume: bool,
    /// `--restart`: wipe state, run from step one.
    pub restart: bool,
    /// `--dry-run`: print the plan without spawning.
    pub dry_run: bool,
    /// `--list`: print plan + last outcome per step. Implies dry-run.
    pub list: bool,
    /// `--update-hooks`: force-refresh the external hook cache.
    pub update_hooks: bool,
    /// Bypass the resume prompt and start fresh. Used by tests and
    /// by the bare `install` invocation when no prior state exists.
    pub assume_fresh: bool,
}

/// Top-level entrypoint. Returns the process exit code; the caller
/// (`ampelos-cli/src/app.rs`) propagates it via `std::process::exit`.
pub async fn run(
    config: Arc<Config>,
    project_root: PathBuf,
    backend: Arc<dyn Backend>,
    plan: Vec<Step>,
    args: InstallArgs,
) -> Result<i32> {
    if args.list {
        return print_list(&project_root, &plan);
    }
    if args.dry_run {
        return print_plan(&plan);
    }

    if args.restart {
        InstallState::wipe(&project_root)?;
    }
    let mut state = match InstallState::load(&project_root)? {
        Some(loaded) if loaded.steps.len() == plan.len() => loaded,
        _ => InstallState::fresh(plan.iter().map(|s| s.name.clone())),
    };
    state.save(&project_root)?;

    // Single-step mode short-circuits all sequencing logic.
    if let Some(name) = args.step.as_deref() {
        return run_single(&config, &project_root, backend, &plan, &mut state, name).await;
    }

    let start_idx = decide_start_index(&plan, &state, &args)?;

    let project_label = config
        .project
        .name
        .clone()
        .unwrap_or_else(|| "ampelos".to_string());
    let mut renderer = Renderer::new(&project_label, &plan)?;
    // Reflect already-completed rows on screen even when starting in
    // the middle of the plan (resume case).
    for (idx, record) in state.steps.iter().enumerate() {
        if record.status == StepStatus::Ok {
            renderer.end(
                idx,
                StepOutcome::Ok,
                Duration::from_millis(record.duration_ms.unwrap_or(0)),
            )?;
        } else if record.status == StepStatus::Skipped {
            renderer.end(
                idx,
                StepOutcome::Skipped,
                Duration::from_millis(record.duration_ms.unwrap_or(0)),
            )?;
        }
    }

    let executor = base_executor(Arc::clone(&config), backend.clone(), &project_root);

    for (idx, step) in plan.iter().enumerate().skip(start_idx) {
        let outcome = execute_step(
            &executor,
            &config,
            &project_root,
            step,
            idx,
            &mut renderer,
            args.update_hooks,
        )
        .await?;
        record_outcome(&mut state, idx, &outcome);
        state.save(&project_root)?;
        match outcome.status {
            StepStatus::Ok | StepStatus::Skipped => continue,
            StepStatus::Failed => {
                renderer.print_failure_summary(&step.name)?;
                return Ok(outcome.exit_code.unwrap_or(1));
            }
            StepStatus::Pending => unreachable!("execute_step never returns Pending"),
        }
    }
    Ok(0)
}

fn decide_start_index(plan: &[Step], state: &InstallState, args: &InstallArgs) -> Result<usize> {
    if args.resume {
        return Ok(state.first_unresolved().unwrap_or(plan.len()));
    }
    if args.assume_fresh {
        return Ok(0);
    }
    // Bare `ampelos install`: if there's any unresolved step from a
    // prior run, surface the choice. We don't prompt here (the runner
    // shouldn't own user I/O); the caller handles the prompt and sets
    // either `resume` or `restart`, or `assume_fresh` for "user opted
    // to start over without wiping state".
    Ok(state.first_unresolved().unwrap_or(0))
}

async fn run_single(
    config: &Config,
    project_root: &Path,
    backend: Arc<dyn Backend>,
    plan: &[Step],
    state: &mut InstallState,
    name: &str,
) -> Result<i32> {
    let (idx, step) = plan
        .iter()
        .enumerate()
        .find(|(_, s)| s.name == name)
        .with_context(|| {
            format!("no install step named `{name}`. Run `ampelos install --list` to see the plan.")
        })?;
    let project_label = config
        .project
        .name
        .clone()
        .unwrap_or_else(|| "ampelos".to_string());
    let mut renderer = Renderer::new(&project_label, std::slice::from_ref(step))?;
    let executor = base_executor(Arc::new(config.clone()), backend, project_root);
    let outcome = execute_step(
        &executor,
        config,
        project_root,
        step,
        0,
        &mut renderer,
        false,
    )
    .await?;
    // Update the matching record in the global state file so the next
    // bare `ampelos install` sees this step as resolved.
    if let Some(rec) = state.find_mut(name) {
        rec.status = outcome.status;
        rec.exit_code = outcome.exit_code;
        rec.duration_ms = outcome.duration.map(|d| d.as_millis() as u64);
        rec.started_at_ms = outcome.started_at_ms;
        rec.ended_at_ms = Some(epoch_ms());
    }
    state.save(project_root)?;
    if matches!(outcome.status, StepStatus::Failed) {
        renderer.print_failure_summary(name)?;
        return Ok(outcome.exit_code.unwrap_or(1));
    }
    let _ = idx; // currently unused but reserved for future positional reporting
    Ok(0)
}

fn base_executor(config: Arc<Config>, backend: Arc<dyn Backend>, project_root: &Path) -> Executor {
    Executor::new(backend, config, project_root)
}

#[derive(Debug)]
struct Outcome {
    status: StepStatus,
    exit_code: Option<i32>,
    duration: Option<Duration>,
    started_at_ms: Option<u64>,
}

fn record_outcome(state: &mut InstallState, idx: usize, o: &Outcome) {
    if let Some(rec) = state.steps.get_mut(idx) {
        rec.status = o.status;
        rec.exit_code = o.exit_code;
        rec.duration_ms = o.duration.map(|d| d.as_millis() as u64);
        rec.started_at_ms = o.started_at_ms;
        rec.ended_at_ms = Some(epoch_ms());
    }
}

async fn execute_step(
    executor: &Executor,
    config: &Config,
    project_root: &Path,
    step: &Step,
    idx: usize,
    renderer: &mut Renderer,
    update_hooks: bool,
) -> Result<Outcome> {
    let started = Instant::now();
    let started_ms = epoch_ms();

    if step.interactive {
        renderer.pause_for_interactive(idx)?;
        let exit = run_interactive(config, project_root, step).await?;
        let duration = started.elapsed();
        let outcome = classify(step.optional, exit, duration, started_ms);
        renderer.resume_after_interactive()?;
        renderer.end(idx, outcome_for_renderer(&outcome), duration)?;
        return Ok(outcome);
    }

    renderer.begin(idx)?;
    let exit = run_captured(executor, config, project_root, step, renderer, update_hooks).await?;
    let duration = started.elapsed();
    let outcome = classify(step.optional, exit, duration, started_ms);
    renderer.end(idx, outcome_for_renderer(&outcome), duration)?;
    Ok(outcome)
}

fn outcome_for_renderer(o: &Outcome) -> StepOutcome {
    match o.status {
        StepStatus::Ok => StepOutcome::Ok,
        StepStatus::Skipped => StepOutcome::Skipped,
        StepStatus::Failed => StepOutcome::Failed,
        StepStatus::Pending => StepOutcome::Failed,
    }
}

fn classify(optional: bool, exit: i32, duration: Duration, started_ms: u64) -> Outcome {
    if exit == 0 {
        return Outcome {
            status: StepStatus::Ok,
            exit_code: Some(0),
            duration: Some(duration),
            started_at_ms: Some(started_ms),
        };
    }
    if optional {
        return Outcome {
            status: StepStatus::Skipped,
            exit_code: Some(exit),
            duration: Some(duration),
            started_at_ms: Some(started_ms),
        };
    }
    Outcome {
        status: StepStatus::Failed,
        exit_code: Some(exit),
        duration: Some(duration),
        started_at_ms: Some(started_ms),
    }
}

/// Execute a step with passthrough stdio. The child inherits the
/// terminal so `ampelos lib ask` & friends Just Work. No output is
/// captured for the failure summary — the user already saw it on
/// screen.
async fn run_interactive(config: &Config, project_root: &Path, step: &Step) -> Result<i32> {
    let env = Env::resolve(config, project_root).await?;
    let (mut cmd, cwd) = build_command(step, project_root, &env).await?;
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    env.apply_to(&mut cmd);
    let status = cmd
        .status()
        .await
        .context("spawn interactive install step")?;
    Ok(status.code().unwrap_or(-1))
}

/// Execute a step with captured stdio: lines flow into the renderer's
/// tail region and into the failure-summary ring buffer. The renderer's
/// spinner ticks on a fixed cadence while we wait.
async fn run_captured(
    executor: &Executor,
    config: &Config,
    project_root: &Path,
    step: &Step,
    renderer: &mut Renderer,
    update_hooks: bool,
) -> Result<i32> {
    match &step.source {
        StepSource::Recipe(name) => run_recipe(executor, name, renderer).await,
        StepSource::Script(script) => run_script(config, project_root, script, renderer).await,
        StepSource::Inline(inline) => run_inline(config, project_root, inline, renderer).await,
        StepSource::InstallHooks => {
            run_install_hooks(config, project_root, renderer, update_hooks).await
        }
        StepSource::ApplyAgents => run_apply_agents(config, project_root, renderer).await,
    }
}

async fn run_recipe(executor: &Executor, name: &str, renderer: &mut Renderer) -> Result<i32> {
    let (sink, mut rx) = ChannelSink::new_pair();
    let exec = executor.clone().with_sink(Arc::new(sink));
    let name = name.to_string();
    let task = tokio::spawn(async move { exec.run_recipe(&name, &[]).await });
    let mut task = std::pin::pin!(task);
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            Some(line) = rx.recv() => {
                renderer.append_tail(&line.line);
            }
            _ = ticker.tick() => {
                renderer.tick()?;
            }
            join = &mut task => {
                let exit = join.context("join recipe task")??;
                // Drain any remaining lines that landed after the task finished.
                while let Ok(line) = rx.try_recv() {
                    renderer.append_tail(&line.line);
                }
                return Ok(exit);
            }
        }
    }
}

async fn run_script(
    config: &Config,
    project_root: &Path,
    script: &InstallStepScript,
    renderer: &mut Renderer,
) -> Result<i32> {
    let env = Env::resolve(config, project_root).await?;
    let cwd = resolve_cwd(script.cwd.as_deref(), project_root);
    let mut cmd = Command::new(&script.path);
    cmd.current_dir(&cwd);
    env.apply_to(&mut cmd);
    for (k, v) in &script.env {
        cmd.env(k, v);
    }
    cmd.env("AMPELOS_PROJECT_DIR", project_root.display().to_string());
    if let Some(parent) = script.path.parent() {
        cmd.env("AMPELOS_SCRIPT_DIR", parent.display().to_string());
    }
    drive_captured(cmd, renderer).await
}

async fn run_inline(
    config: &Config,
    project_root: &Path,
    inline: &InlineStep,
    renderer: &mut Renderer,
) -> Result<i32> {
    let env = Env::resolve(config, project_root).await?;
    let cwd = resolve_cwd(inline.cwd.as_deref(), project_root);
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&inline.run);
    cmd.current_dir(&cwd);
    env.apply_to(&mut cmd);
    for (k, v) in &inline.env {
        cmd.env(k, v);
    }
    cmd.env("AMPELOS_PROJECT_DIR", project_root.display().to_string());
    drive_captured(cmd, renderer).await
}

/// Build a tokio Command for `step` plus the cwd it should run in.
/// Used by the interactive path; the captured path uses
/// `run_script` / `run_inline` directly so it can wire stdio pipes.
async fn build_command(step: &Step, project_root: &Path, _env: &Env) -> Result<(Command, PathBuf)> {
    match &step.source {
        StepSource::Inline(inline) => {
            let cwd = resolve_cwd(inline.cwd.as_deref(), project_root);
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&inline.run);
            for (k, v) in &inline.env {
                cmd.env(k, v);
            }
            Ok((cmd, cwd))
        }
        StepSource::Script(script) => {
            let cwd = resolve_cwd(script.cwd.as_deref(), project_root);
            let mut cmd = Command::new(&script.path);
            for (k, v) in &script.env {
                cmd.env(k, v);
            }
            if let Some(parent) = script.path.parent() {
                cmd.env("AMPELOS_SCRIPT_DIR", parent.display().to_string());
            }
            Ok((cmd, cwd))
        }
        StepSource::Recipe(_) | StepSource::InstallHooks | StepSource::ApplyAgents => {
            // None of these carry an interactive flag (recipes don't,
            // and the two builtins are always non-interactive). Reach
            // here only if someone hand-edits the plan.
            anyhow::bail!("step kind cannot run interactively");
        }
    }
}

async fn drive_captured(mut cmd: Command, renderer: &mut Renderer) -> Result<i32> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawn install step")?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let tx_err = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(line);
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_err.send(line);
        }
    });

    let mut wait = std::pin::pin!(child.wait());
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            Some(line) = rx.recv() => {
                renderer.append_tail(&line);
            }
            _ = ticker.tick() => {
                renderer.tick()?;
            }
            status = &mut wait => {
                let status = status.context("wait child")?;
                while let Ok(line) = rx.try_recv() {
                    renderer.append_tail(&line);
                }
                return Ok(status.code().unwrap_or(-1));
            }
        }
    }
}

async fn run_install_hooks(
    config: &Config,
    project_root: &Path,
    renderer: &mut Renderer,
    force_update: bool,
) -> Result<i32> {
    let pre_commit_path = project_root.join(".pre-commit-config.yaml");
    if pre_commit_path.is_file() {
        match crate::hooks::config::load_from_path(&pre_commit_path) {
            Ok(pcconf) => {
                for repo in &pcconf.repos {
                    if repo.is_local() {
                        continue;
                    }
                    if repo.is_meta() {
                        renderer.append_tail(&format!(
                            "skipping `repo: meta` ({} hooks) — not supported by ampelos",
                            repo.hooks.len()
                        ));
                        continue;
                    }
                    let rev = repo.rev.as_deref().unwrap_or("<unpinned>");
                    renderer.append_tail(&format!("fetching {} @ {}", repo.repo, rev));
                    if let Err(e) =
                        crate::hooks::cache::clone_or_reuse(project_root, repo, force_update).await
                    {
                        renderer.append_tail(&format!("  warn: {e}"));
                    }
                }
            }
            Err(e) => {
                renderer.append_tail(&format!(
                    "warning: failed to read .pre-commit-config.yaml: {e}"
                ));
            }
        }
    }

    let stages = default_install_stages(config);
    renderer.append_tail(&format!("installing shims for: {}", stages.join(", ")));
    let installed =
        crate::hooks::installer::install(project_root, &stages).context("install hook shims")?;
    for path in installed {
        renderer.append_tail(&format!("  wrote {}", path.display()));
    }
    Ok(0)
}

async fn run_apply_agents(
    config: &Config,
    project_root: &Path,
    renderer: &mut Renderer,
) -> Result<i32> {
    let opts = crate::agents::ApplyOptions::default();
    let report = match crate::agents::apply(project_root, &config.agents, &opts).await {
        Ok(r) => r,
        Err(e) => {
            renderer.append_tail(&format!("agents apply failed: {e}"));
            return Ok(1);
        }
    };
    for dest in &report.written {
        renderer.append_tail(&format!("  wrote   {}", dest.display()));
    }
    for dest in &report.updated {
        renderer.append_tail(&format!("  updated {}", dest.display()));
    }
    for dest in &report.removed {
        renderer.append_tail(&format!("  removed {}", dest.display()));
    }
    for dest in &report.once_kept {
        renderer.append_tail(&format!("  kept    {} (mode = once)", dest.display()));
    }
    for collision in &report.collisions {
        renderer.append_tail(&format!(
            "  warn    {} declared by [{}], using `{}`",
            collision.dest.display(),
            collision.overshadowed_sources.join(", "),
            collision.winning_source,
        ));
    }
    for entry in &report.drift_warnings {
        renderer.append_tail(&format!(
            "  warn    {} drifted; left alone",
            entry.dest.display(),
        ));
    }
    if report.written.is_empty() && report.updated.is_empty() && report.removed.is_empty() {
        renderer.append_tail("  agent files unchanged");
    }
    Ok(0)
}

/// Default hook stages installed when no explicit list is given.
/// Mirrors the existing `commands::hooks::default_install_stages`
/// rules so the install flow and the explicit `ampelos hooks install`
/// flow agree.
fn default_install_stages(config: &Config) -> Vec<&'static str> {
    let mut stages = vec!["pre-commit"];
    if config.worktrees.dotenv.is_some() {
        stages.push("post-checkout");
        stages.push("post-merge");
    }
    stages
}

fn print_plan(plan: &[Step]) -> Result<i32> {
    println!("Plan:");
    for (idx, step) in plan.iter().enumerate() {
        let kind = match step.source {
            StepSource::Recipe(_) => "recipe",
            StepSource::Script(_) => "script",
            StepSource::Inline(_) => "inline",
            StepSource::InstallHooks => "builtin",
            StepSource::ApplyAgents => "builtin",
        };
        let flags = match (step.optional, step.interactive) {
            (true, true) => " [optional, interactive]",
            (true, false) => " [optional]",
            (false, true) => " [interactive]",
            (false, false) => "",
        };
        println!("  {:>2}. {} ({}){}", idx + 1, step.label(), kind, flags);
    }
    Ok(0)
}

fn print_list(project_root: &Path, plan: &[Step]) -> Result<i32> {
    let state = InstallState::load(project_root)?;
    println!("Install plan:");
    for (idx, step) in plan.iter().enumerate() {
        let last = state
            .as_ref()
            .and_then(|s| s.steps.get(idx))
            .map(|r| match r.status {
                StepStatus::Ok => "ok",
                StepStatus::Failed => "failed",
                StepStatus::Skipped => "skipped",
                StepStatus::Pending => "pending",
            })
            .unwrap_or("not run");
        println!("  {:>2}. {:30} {}", idx + 1, step.label(), last);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_zero_exit_is_ok() {
        let o = classify(false, 0, Duration::from_secs(1), 0);
        assert_eq!(o.status, StepStatus::Ok);
    }

    #[test]
    fn classify_nonzero_required_is_failed() {
        let o = classify(false, 1, Duration::from_secs(1), 0);
        assert_eq!(o.status, StepStatus::Failed);
    }

    #[test]
    fn classify_nonzero_optional_is_skipped() {
        let o = classify(true, 1, Duration::from_secs(1), 0);
        assert_eq!(o.status, StepStatus::Skipped);
    }
}
