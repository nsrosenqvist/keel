//! Hook execution.
//!
//! Two paths:
//!
//! 1. **Native** (`repo: local` + `language: system | script`): the entry
//!    is parsed via `shell_words`, args are appended, and — unless
//!    `pass_filenames = false` — the matching staged files come last.
//!    The hook runs as a child process inheriting stdio.
//! 2. **Bridged** (anything else): if the `pre-commit` binary is on PATH,
//!    we shell out to it for that hook. If not, we WARN and skip the hook
//!    rather than failing the whole stage. The user can install
//!    `pre-commit` to get full coverage.

use crate::config::{HookLanguage, HookSpec, PreCommitConfig, Repo};
use crate::error::HookError;
use crate::git;
use regex::Regex;
use std::path::Path;
use tokio::process::Command;
use tracing::{debug, warn};

/// Outcome of running a single hook.
#[derive(Debug)]
pub struct HookOutcome {
    pub hook_id: String,
    pub native: bool,
    pub skipped: Option<String>,
    pub exit_code: Option<i32>,
}

impl HookOutcome {
    pub fn passed(&self) -> bool {
        self.skipped.is_some() || matches!(self.exit_code, Some(0))
    }
}

/// Run the pre-commit stage. Returns the per-hook outcomes; the caller
/// renders them and decides on the overall exit code.
pub async fn run_pre_commit(
    config: &PreCommitConfig,
    project_root: &Path,
    stage: &str,
) -> Result<Vec<HookOutcome>, HookError> {
    let repo_root = git::discover_repo(project_root)?;
    let staged = git::staged_files(&repo_root).await?;

    let mut outcomes = Vec::new();
    for repo in &config.repos {
        for hook in &repo.hooks {
            if !hook.applies_to_stage(stage, &config.default_stages) {
                continue;
            }
            let outcome = run_one_hook(repo, hook, &staged, &repo_root).await?;
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

async fn run_one_hook(
    repo: &Repo,
    hook: &HookSpec,
    staged: &[String],
    repo_root: &Path,
) -> Result<HookOutcome, HookError> {
    if repo.is_local() && hook.language.is_native() {
        return run_native(hook, staged, repo_root).await;
    }
    bridge_to_pre_commit(hook, repo_root).await
}

async fn run_native(
    hook: &HookSpec,
    staged: &[String],
    repo_root: &Path,
) -> Result<HookOutcome, HookError> {
    let entry = hook.entry.as_ref().ok_or_else(|| HookError::EntryMissing {
        hook: hook.id.clone(),
    })?;
    let mut argv = shell_words::split(entry).map_err(|e| HookError::EntryParse {
        hook: hook.id.clone(),
        message: e.to_string(),
    })?;
    if argv.is_empty() {
        return Err(HookError::EntryMissing {
            hook: hook.id.clone(),
        });
    }
    argv.extend(hook.args.iter().cloned());

    let files = filter_files(hook, staged)?;
    if hook.pass_filenames && files.is_empty() && !hook.always_run {
        debug!(
            hook = hook.id,
            "no staged files match; skipping native hook"
        );
        return Ok(HookOutcome {
            hook_id: hook.id.clone(),
            native: true,
            skipped: Some("no matching staged files".into()),
            exit_code: None,
        });
    }
    if hook.pass_filenames {
        argv.extend(files.iter().cloned());
    }

    let (program, rest) = argv.split_first().expect("argv non-empty above");
    let mut cmd = Command::new(program);
    cmd.args(rest);
    cmd.current_dir(repo_root);
    let status = cmd
        .status()
        .await
        .map_err(|e| HookError::GitFailed(format!("spawn `{program}`: {e}")))?;
    Ok(HookOutcome {
        hook_id: hook.id.clone(),
        native: true,
        skipped: None,
        exit_code: status.code(),
    })
}

async fn bridge_to_pre_commit(hook: &HookSpec, repo_root: &Path) -> Result<HookOutcome, HookError> {
    if which::which("pre-commit").is_err() {
        let reason = match hook.language {
            HookLanguage::Other => {
                "language not recognised; install pre-commit binary for full support".to_string()
            }
            ref other => {
                format!("{other:?} hooks need the pre-commit binary; install it to enable")
            }
        };
        warn!(hook = hook.id, "skipping: {reason}");
        return Ok(HookOutcome {
            hook_id: hook.id.clone(),
            native: false,
            skipped: Some(reason),
            exit_code: None,
        });
    }
    let status = Command::new("pre-commit")
        .args(["run", "--hook-stage", "pre-commit", &hook.id])
        .current_dir(repo_root)
        .status()
        .await
        .map_err(|e| HookError::GitFailed(format!("spawn pre-commit: {e}")))?;
    Ok(HookOutcome {
        hook_id: hook.id.clone(),
        native: false,
        skipped: None,
        exit_code: status.code(),
    })
}

fn filter_files(hook: &HookSpec, staged: &[String]) -> Result<Vec<String>, HookError> {
    let include = hook
        .files
        .as_ref()
        .map(|p| compile(&hook.id, p))
        .transpose()?;
    let exclude = hook
        .exclude
        .as_ref()
        .map(|p| compile(&hook.id, p))
        .transpose()?;

    Ok(staged
        .iter()
        .filter(|f| include.as_ref().is_none_or(|r| r.is_match(f)))
        .filter(|f| exclude.as_ref().is_none_or(|r| !r.is_match(f)))
        .cloned()
        .collect())
}

fn compile(hook_id: &str, pattern: &str) -> Result<Regex, HookError> {
    Regex::new(pattern).map_err(|source| HookError::InvalidRegex {
        hook: hook_id.to_string(),
        pattern: pattern.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookLanguage;

    fn hook(files: Option<&str>, exclude: Option<&str>) -> HookSpec {
        HookSpec {
            id: "t".into(),
            name: None,
            language: HookLanguage::System,
            entry: Some("echo".into()),
            args: vec![],
            files: files.map(String::from),
            exclude: exclude.map(String::from),
            pass_filenames: true,
            stages: vec![],
            always_run: false,
        }
    }

    #[test]
    fn filter_files_includes_only_matching() {
        let h = hook(Some(r"\.rs$"), None);
        let files = vec![
            "src/main.rs".to_string(),
            "README.md".into(),
            "src/lib.rs".into(),
        ];
        let kept = filter_files(&h, &files).unwrap();
        assert_eq!(kept, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn filter_files_applies_exclude_after_include() {
        let h = hook(Some(r"\.rs$"), Some(r"^vendor/"));
        let files = vec![
            "src/lib.rs".into(),
            "vendor/x.rs".into(),
            "src/main.rs".into(),
        ];
        let kept = filter_files(&h, &files).unwrap();
        assert_eq!(kept, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn invalid_regex_errors() {
        let h = hook(Some(r"["), None);
        let err = filter_files(&h, &[]).unwrap_err();
        assert!(matches!(err, HookError::InvalidRegex { .. }));
    }

    #[test]
    fn hook_outcome_passed_for_zero_or_skip() {
        let pass = HookOutcome {
            hook_id: "x".into(),
            native: true,
            skipped: None,
            exit_code: Some(0),
        };
        let skip = HookOutcome {
            hook_id: "x".into(),
            native: false,
            skipped: Some("reason".into()),
            exit_code: None,
        };
        let fail = HookOutcome {
            hook_id: "x".into(),
            native: true,
            skipped: None,
            exit_code: Some(1),
        };
        assert!(pass.passed());
        assert!(skip.passed());
        assert!(!fail.passed());
    }
}
