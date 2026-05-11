//! Hook execution.
//!
//! Two paths, both native:
//!
//! 1. **Local hook** (`repo: local`, any `language: system | script`):
//!    the user's `HookSpec` provides everything. The entry is parsed
//!    via `shell_words`, args are appended, and — unless
//!    `pass_filenames = false` — the matching staged files come last.
//! 2. **Cached upstream repo** (`repo: <url>` with a `rev`): keel
//!    clones the repo into `.keel/cache/hooks/<key>/` and reads its
//!    `.pre-commit-hooks.yaml` to fill in any fields the user didn't
//!    override. Same native execution path after merging.
//!
//! Unsupported shapes (`language: python` / `node` / `ruby` / …,
//! and `repo: meta`) produce a clear error at run time rather than
//! silently skipping. keel deliberately does not bridge to the
//! `pre-commit` binary; replication is the only mode.

use crate::hooks::cache;
use crate::hooks::config::{HookLanguage, HookSpec, PreCommitConfig, Repo, UpstreamHook};
use crate::hooks::error::HookError;
use crate::hooks::git;
use regex::Regex;
use std::path::Path;
use tokio::process::Command;
use tracing::debug;

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

/// Run every hook configured for `stage`. Returns the per-hook
/// outcomes in declaration order; the caller renders them and decides
/// on the overall exit code (typically: any non-zero hook fails the
/// stage).
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
            let outcome = run_one_hook(project_root, repo, hook, &staged, &repo_root).await?;
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

async fn run_one_hook(
    project_root: &Path,
    repo: &Repo,
    hook: &HookSpec,
    staged: &[String],
    repo_root: &Path,
) -> Result<HookOutcome, HookError> {
    if repo.is_meta() {
        return Err(HookError::MetaRepoNotSupported);
    }

    let resolved = if repo.is_local() {
        if !hook.language.is_native() {
            return Err(HookError::UnsupportedLanguage {
                hook: hook.id.clone(),
                language: format!("{:?}", hook.language),
            });
        }
        ResolvedHook::local(hook.clone())
    } else {
        let cached = cache::clone_or_reuse(project_root, repo, false).await?;
        let upstream = find_upstream(&cached.clone_dir, hook, &repo.repo, repo.rev.as_deref())?;
        let merged = crate::hooks::config::merge_with_upstream(hook, &upstream)?;
        ResolvedHook::cached(merged, cached.clone_dir)
    };

    run_native(&resolved, staged, repo_root).await
}

/// What the runner actually executes after resolving local-vs-cached
/// and merging upstream-vs-user fields.
struct ResolvedHook {
    spec: HookSpec,
    /// Directory the entry path resolves against (for `language: script`
    /// hooks whose entry is a file inside the cached repo). For
    /// `language: system` and local hooks this is unused.
    script_root: Option<std::path::PathBuf>,
}

impl ResolvedHook {
    fn local(spec: HookSpec) -> Self {
        Self {
            spec,
            script_root: None,
        }
    }

    fn cached(spec: HookSpec, clone_dir: std::path::PathBuf) -> Self {
        Self {
            spec,
            script_root: Some(clone_dir),
        }
    }
}

fn find_upstream(
    clone_dir: &Path,
    hook: &HookSpec,
    repo_url: &str,
    rev: Option<&str>,
) -> Result<UpstreamHook, HookError> {
    let hooks = crate::hooks::config::load_upstream_hooks(clone_dir)?;
    hooks
        .into_iter()
        .find(|h| h.id == hook.id)
        .ok_or_else(|| HookError::UpstreamHookMissing {
            repo: repo_url.to_string(),
            rev: rev.unwrap_or("").to_string(),
            hook: hook.id.clone(),
        })
}

async fn run_native(
    resolved: &ResolvedHook,
    staged: &[String],
    repo_root: &Path,
) -> Result<HookOutcome, HookError> {
    let entry = resolved
        .spec
        .entry
        .as_ref()
        .ok_or_else(|| HookError::EntryMissing {
            hook: resolved.spec.id.clone(),
        })?;
    let mut argv = shell_words::split(entry).map_err(|e| HookError::EntryParse {
        hook: resolved.spec.id.clone(),
        message: e.to_string(),
    })?;
    if argv.is_empty() {
        return Err(HookError::EntryMissing {
            hook: resolved.spec.id.clone(),
        });
    }

    // For `language: script`, the entry is a path *inside the cloned
    // repo*. Rewrite the first token to its absolute path so we can
    // still set `current_dir` to the user's repo_root (matches
    // pre-commit's semantics: hooks see the project tree, but their
    // own files come from the cache).
    if matches!(resolved.spec.language, HookLanguage::Script)
        && let Some(root) = resolved.script_root.as_deref()
    {
        let candidate = root.join(&argv[0]);
        if candidate.is_file() {
            argv[0] = candidate.to_string_lossy().into_owned();
        }
    }

    argv.extend(resolved.spec.args.iter().cloned());

    let files = filter_files(&resolved.spec, staged)?;
    if resolved.spec.pass_filenames && files.is_empty() && !resolved.spec.always_run {
        debug!(
            hook = resolved.spec.id,
            "no staged files match; skipping native hook"
        );
        return Ok(HookOutcome {
            hook_id: resolved.spec.id.clone(),
            native: true,
            skipped: Some("no matching staged files".into()),
            exit_code: None,
        });
    }
    if resolved.spec.pass_filenames {
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
        hook_id: resolved.spec.id.clone(),
        native: true,
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
    use crate::hooks::config::HookLanguage;

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

    #[tokio::test]
    async fn meta_repo_errors_at_run_time() {
        let temp = tempfile::TempDir::new().unwrap();
        // Make it a git repo so git::discover_repo succeeds.
        tokio::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .await
            .unwrap();
        let cfg = PreCommitConfig {
            repos: vec![Repo {
                repo: "meta".into(),
                rev: None,
                hooks: vec![HookSpec {
                    id: "identity".into(),
                    name: None,
                    language: HookLanguage::System,
                    entry: Some("true".into()),
                    args: vec![],
                    files: None,
                    exclude: None,
                    pass_filenames: false,
                    stages: vec![],
                    always_run: true,
                }],
            }],
            default_stages: vec![],
        };
        let err = run_pre_commit(&cfg, temp.path(), "pre-commit")
            .await
            .unwrap_err();
        assert!(matches!(err, HookError::MetaRepoNotSupported));
    }

    #[tokio::test]
    async fn local_python_hook_errors_with_clear_message() {
        let temp = tempfile::TempDir::new().unwrap();
        tokio::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .await
            .unwrap();
        let cfg = PreCommitConfig {
            repos: vec![Repo {
                repo: "local".into(),
                rev: None,
                hooks: vec![HookSpec {
                    id: "ruff".into(),
                    name: None,
                    language: HookLanguage::Python,
                    entry: Some("ruff".into()),
                    args: vec![],
                    files: None,
                    exclude: None,
                    pass_filenames: true,
                    stages: vec![],
                    always_run: true,
                }],
            }],
            default_stages: vec![],
        };
        let err = run_pre_commit(&cfg, temp.path(), "pre-commit")
            .await
            .unwrap_err();
        match err {
            HookError::UnsupportedLanguage { hook, .. } => assert_eq!(hook, "ruff"),
            other => panic!("expected UnsupportedLanguage, got {other:?}"),
        }
    }
}
