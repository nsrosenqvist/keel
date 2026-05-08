//! `scaffl hooks <subcommand>` — install / run / uninstall git hooks.
//!
//! The CLI orchestrates two sources of hook logic:
//!
//! - Native scaffl hooks declared in `scaffl.toml` `[hooks.<stage>]` —
//!   these run as recipes through [`scaffl_runtime::Executor`].
//! - `.pre-commit-config.yaml` hooks — these go through `scaffl_hooks`,
//!   which natively runs `repo: local` + `language: system | script` and
//!   bridges everything else to the `pre-commit` binary.

use anyhow::{Context, Result};
use scaffl_config::Config;
use scaffl_container::{Backend, compose::ComposeBackend};
use scaffl_hooks::{HookOutcome, installer};
use scaffl_runtime::Executor;
use std::path::Path;
use std::sync::Arc;

pub async fn install(project_root: &Path, stages: &[String]) -> Result<()> {
    let stages_ref: Vec<&str> = if stages.is_empty() {
        vec!["pre-commit"]
    } else {
        stages.iter().map(String::as_str).collect()
    };
    let written = installer::install(project_root, &stages_ref)
        .with_context(|| format!("install hooks under {}", project_root.display()))?;
    for path in written {
        println!("Installed {}", path.display());
    }
    Ok(())
}

pub async fn uninstall(project_root: &Path, stages: &[String]) -> Result<()> {
    let stages_ref: Vec<&str> = if stages.is_empty() {
        installer::KNOWN_STAGES.to_vec()
    } else {
        stages.iter().map(String::as_str).collect()
    };
    let removed = installer::uninstall(project_root, &stages_ref)
        .with_context(|| format!("uninstall hooks under {}", project_root.display()))?;
    if removed.is_empty() {
        println!("No scaffl-managed hooks to remove.");
    } else {
        for path in removed {
            println!("Removed {}", path.display());
        }
    }
    Ok(())
}

pub async fn run(config: &Arc<Config>, project_root: &Path, stage: &str) -> Result<i32> {
    let mut overall: i32 = 0;

    // 1. Native scaffl hooks.
    let native_hooks = config.hooks.for_stage(stage);
    if !native_hooks.is_empty() {
        let backend = build_backend_or_null(config).await;
        let executor = Executor::new(backend, Arc::clone(config), project_root);
        for name in native_hooks {
            println!("[scaffl] {name}");
            let code = executor.run_recipe(name, &[]).await?;
            if code != 0 && overall == 0 {
                overall = code;
            }
            if code != 0 {
                eprintln!("[scaffl] {name}: failed (exit {code})");
                break; // mirror pre-commit's stop-on-first-failure behaviour
            }
        }
    }

    // 2. .pre-commit-config.yaml.
    let pcc_path = project_root.join(".pre-commit-config.yaml");
    if pcc_path.exists() && overall == 0 {
        let pcc = scaffl_hooks::config::load_from_path(&pcc_path)
            .with_context(|| format!("load {}", pcc_path.display()))?;
        let outcomes = scaffl_hooks::run_pre_commit(&pcc, project_root, stage)
            .await
            .context("run pre-commit hooks")?;
        for outcome in &outcomes {
            print_outcome(outcome);
            if let Some(code) = outcome.exit_code
                && code != 0
                && overall == 0
            {
                overall = code;
            }
        }
    }

    if native_hooks.is_empty() && !pcc_path.exists() {
        println!("(no hooks configured for stage `{stage}`)");
    }

    Ok(overall)
}

fn print_outcome(outcome: &HookOutcome) {
    let kind = if outcome.native {
        "[native]"
    } else {
        "[bridge]"
    };
    if let Some(reason) = &outcome.skipped {
        println!("{kind} {} skipped: {reason}", outcome.hook_id);
    } else {
        let code = outcome.exit_code.unwrap_or(-1);
        let status = if code == 0 { "ok" } else { "FAIL" };
        println!("{kind} {} {status} (exit {code})", outcome.hook_id);
    }
}

async fn build_backend_or_null(config: &Config) -> Arc<dyn Backend> {
    use scaffl_config::model::Backend as B;
    match config.runtime.backend {
        B::Compose => match ComposeBackend::detect().await {
            Ok(b) => Arc::new(b),
            Err(_) => Arc::new(scaffl_container::null::NullBackend),
        },
        _ => Arc::new(scaffl_container::null::NullBackend),
    }
}
