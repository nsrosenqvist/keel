//! `ampelos agents` subcommand: manage upstream-sourced agent
//! instructions and skills.
//!
//! Thin CLI wrapper around `crate::agents::apply` / `detect_drift`.
//! All policy lives in the `ampelos-agents` crate; this module
//! formats arguments + report output.

use crate::agents::{ApplyOptions, ApplyReport, apply, detect_drift};
use crate::config::Config;
use anyhow::{Context, Result};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use std::path::Path;

/// Run `ampelos agents install`. Mirrors `ampelos agents update` when
/// state already exists; the difference is just intent.
pub async fn install(
    config: &Config,
    project_root: &Path,
    force: bool,
    dry_run: bool,
    force_overwrite_drift: bool,
) -> Result<()> {
    let opts = ApplyOptions {
        force,
        dry_run,
        force_overwrite_drift,
        source_filter: None,
    };
    let report = apply(project_root, &config.agents, &opts)
        .await
        .context("apply agents")?;
    print_report("install", &report);
    Ok(())
}

/// Run `ampelos agents update`. Same pipeline as install, plus an
/// optional source filter and an implicit re-fetch for floating refs
/// (handled inside `crate::agents::apply`).
pub async fn update(
    config: &Config,
    project_root: &Path,
    sources: Vec<String>,
    force: bool,
    dry_run: bool,
    force_overwrite_drift: bool,
) -> Result<()> {
    let opts = ApplyOptions {
        force,
        dry_run,
        force_overwrite_drift,
        source_filter: if sources.is_empty() {
            None
        } else {
            Some(sources)
        },
    };
    let report = apply(project_root, &config.agents, &opts)
        .await
        .context("apply agents")?;
    print_report("update", &report);
    Ok(())
}

/// Run `ampelos agents diff` — equivalent to apply with `dry_run = true`.
pub async fn diff(config: &Config, project_root: &Path) -> Result<()> {
    let opts = ApplyOptions {
        dry_run: true,
        ..Default::default()
    };
    let report = apply(project_root, &config.agents, &opts).await?;
    if report.written.is_empty()
        && report.updated.is_empty()
        && report.removed.is_empty()
        && report.drift_warnings.is_empty()
    {
        println!("agents: nothing to do");
        return Ok(());
    }
    for dest in &report.written {
        println!("would write   {}", dest.display());
    }
    for dest in &report.updated {
        println!("would update  {}", dest.display());
    }
    for dest in &report.removed {
        println!("would remove  {}", dest.display());
    }
    for entry in &report.drift_warnings {
        println!(
            "would skip    {} (drifted; use --force-overwrite-drift to overwrite)",
            entry.dest.display()
        );
    }
    Ok(())
}

/// Run `ampelos agents status`. Read-only — never touches the cache or
/// upstream. Reports per-source pinned rev + per-file drift state
/// from the on-disk `agents.state.json` and current file contents.
pub async fn status(config: &Config, project_root: &Path, strict: bool) -> Result<i32> {
    let state = crate::agents::AgentsState::load(project_root)?.unwrap_or_default();

    let mut sources_table = Table::new();
    sources_table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "source",
            "rev_request",
            "resolved_sha",
            "manifest_sha",
        ]);
    if config.agents.sources.is_empty() && state.sources.is_empty() {
        println!("agents: no sources declared");
    } else {
        for declared in &config.agents.sources {
            let recorded = state.sources.iter().find(|s| s.name == declared.name);
            sources_table.add_row(vec![
                declared.name.clone(),
                declared.rev.clone(),
                recorded
                    .map(|r| short_sha(&r.resolved_sha))
                    .unwrap_or_else(|| "(not applied)".into()),
                recorded
                    .map(|r| short_sha(&r.manifest_sha256))
                    .unwrap_or_default(),
            ]);
        }
        println!("{sources_table}");
    }

    let drift = detect_drift(&state, project_root)?;
    let drifted: std::collections::HashSet<_> = drift.iter().map(|d| d.dest.clone()).collect();
    let mut files_table = Table::new();
    files_table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["dest", "source", "status"]);
    let mut bad = false;
    for file in &state.files {
        let abs = project_root.join(&file.dest);
        let status = if !abs.exists() {
            bad = true;
            "missing"
        } else if drifted.contains(&file.dest) {
            bad = true;
            "drifted"
        } else {
            "ok"
        };
        files_table.add_row(vec![
            file.dest.display().to_string(),
            file.source_name.clone(),
            status.into(),
        ]);
    }
    if !state.files.is_empty() {
        println!("{files_table}");
    }

    Ok(if strict && bad { 1 } else { 0 })
}

fn print_report(verb: &str, report: &ApplyReport) {
    let prefix = if report.dry_run { "would " } else { "" };
    let mut printed = false;
    for dest in &report.written {
        println!("agents {verb}: {prefix}wrote   {}", dest.display());
        printed = true;
    }
    for dest in &report.updated {
        println!("agents {verb}: {prefix}updated {}", dest.display());
        printed = true;
    }
    for dest in &report.removed {
        println!("agents {verb}: {prefix}removed {}", dest.display());
        printed = true;
    }
    for dest in &report.once_kept {
        println!("agents {verb}: kept    {} (mode = once)", dest.display());
        printed = true;
    }
    for collision in &report.collisions {
        println!(
            "agents {verb}: warn    {} declared by [{} -> {}], later wins",
            collision.dest.display(),
            collision.overshadowed_sources.join(", "),
            collision.winning_source,
        );
        printed = true;
    }
    for entry in &report.drift_warnings {
        println!(
            "agents {verb}: warn    {} drifted; left alone (use --force-overwrite-drift to overwrite)",
            entry.dest.display(),
        );
        printed = true;
    }
    if !printed {
        println!("agents {verb}: nothing to do");
    }
}

fn short_sha(sha: &str) -> String {
    if sha.len() <= 12 {
        return sha.to_string();
    }
    sha[..12].to_string()
}
