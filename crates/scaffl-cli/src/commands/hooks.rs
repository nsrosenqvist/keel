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

pub async fn install(config: &Config, project_root: &Path, stages: &[String]) -> Result<()> {
    let stages_ref: Vec<&str> = if stages.is_empty() {
        default_install_stages(config)
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

/// Default stages for `scaffl hooks install` (no `--stages`):
/// always includes `pre-commit`; adds `post-checkout` and
/// `post-merge` when `[worktrees] dotenv` is set so the file stays
/// fresh after branch switches even when the user runs Docker
/// directly.
fn default_install_stages(config: &Config) -> Vec<&'static str> {
    let mut stages = vec!["pre-commit"];
    if config.worktrees.dotenv.is_some() {
        stages.push("post-checkout");
        stages.push("post-merge");
    }
    stages
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
        // The dotenv auto-write runs at app entry (before this
        // function), so for the stages it's wired to we want to
        // surface that as the actual work — not claim "nothing
        // happened".
        if dotenv_stage_handled(config, stage) {
            println!("[scaffl] refreshed dotenv for stage `{stage}`");
        } else {
            println!("(no hooks configured for stage `{stage}`)");
        }
    }

    Ok(overall)
}

/// Returns true when `[worktrees] dotenv` is set and the stage is one
/// the auto-write wiring covers — used to phrase the no-other-hooks
/// message correctly instead of saying "nothing happened".
fn dotenv_stage_handled(config: &Config, stage: &str) -> bool {
    config.worktrees.dotenv.is_some() && matches!(stage, "post-checkout" | "post-merge")
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
    match config.containers.backend {
        B::Compose => match ComposeBackend::detect().await {
            Ok(b) => Arc::new(b),
            Err(_) => Arc::new(scaffl_container::null::NullBackend),
        },
        _ => Arc::new(scaffl_container::null::NullBackend),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_dotenv(dotenv: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.worktrees.dotenv = dotenv.map(String::from);
        cfg
    }

    #[test]
    fn default_install_stages_minimal() {
        let cfg = cfg_with_dotenv(None);
        assert_eq!(default_install_stages(&cfg), vec!["pre-commit"]);
    }

    #[test]
    fn default_install_stages_with_dotenv_includes_checkout_and_merge() {
        let cfg = cfg_with_dotenv(Some(".env"));
        assert_eq!(
            default_install_stages(&cfg),
            vec!["pre-commit", "post-checkout", "post-merge"]
        );
    }

    #[test]
    fn dotenv_stage_handled_only_for_relevant_stages() {
        let cfg = cfg_with_dotenv(Some(".env"));
        assert!(dotenv_stage_handled(&cfg, "post-checkout"));
        assert!(dotenv_stage_handled(&cfg, "post-merge"));
        assert!(!dotenv_stage_handled(&cfg, "pre-commit"));
        assert!(!dotenv_stage_handled(&cfg, "pre-push"));
    }

    #[test]
    fn dotenv_stage_handled_off_when_unset() {
        let cfg = cfg_with_dotenv(None);
        assert!(!dotenv_stage_handled(&cfg, "post-checkout"));
    }
}
