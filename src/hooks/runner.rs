//! Hook execution.
//!
//! Two parse paths, one execution path:
//!
//! 1. **Local hook** (`repo: local`): the user's `HookSpec` provides
//!    everything. The entry is parsed via `shell_words`, args are
//!    appended, and — unless `pass_filenames = false` — the matching
//!    staged files come last.
//! 2. **Cached upstream repo** (`repo: <url>` with a `rev`): keel
//!    clones the repo into `.keel/cache/hooks/<key>/` and reads its
//!    `.pre-commit-hooks.yaml` to fill in any fields the user didn't
//!    override. Same execution path after merging.
//!
//! The hook's `language` tag is **advisory**: keel runs the entry
//! verbatim regardless of `python` / `node` / `ruby` / … and trusts
//! the runtime to be on `PATH` (typically installed by `keel install`).
//! The only shape we still reject at run-time is `repo: meta`, which
//! references pre-commit's built-in hooks that we deliberately don't
//! implement. keel does not bridge to the `pre-commit` binary;
//! replication is the only mode.
//!
//! Execution dispatch (per hook, after the spec is resolved):
//!
//! - `in = "<service>"` → exec inside that container service via the
//!   configured [`Backend`](crate::container::Backend).
//! - else, when the executor's workspace target is
//!   [`crate::runtime::executor::WorkspaceTarget::Devcontainer`] →
//!   exec inside the devcontainer.
//! - else → spawn on the host with cwd = git repo root.

use crate::hooks::cache;
use crate::hooks::config::{HookLanguage, HookSpec, PreCommitConfig, Repo, UpstreamHook};
use crate::hooks::error::HookError;
use crate::hooks::git;
use crate::runtime::Executor;
use regex::Regex;
use std::path::Path;
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
///
/// `executor` provides the container/devcontainer dispatch — hooks
/// with `in = "<service>"` exec inside that service, otherwise
/// hooks land on the executor's workspace target (host, or the
/// devcontainer when `[devcontainer] enabled = true`).
pub async fn run_pre_commit(
    config: &PreCommitConfig,
    project_root: &Path,
    stage: &str,
    executor: &Executor,
) -> Result<Vec<HookOutcome>, HookError> {
    let repo_root = git::discover_repo(project_root)?;
    let staged = git::staged_files(&repo_root).await?;

    let mut outcomes = Vec::new();
    for repo in &config.repos {
        for hook in &repo.hooks {
            if !hook.applies_to_stage(stage, &config.default_stages) {
                continue;
            }
            let outcome =
                run_one_hook(project_root, repo, hook, &staged, &repo_root, executor).await?;
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
    executor: &Executor,
) -> Result<HookOutcome, HookError> {
    if repo.is_meta() {
        return Err(HookError::MetaRepoNotSupported);
    }

    let resolved = if repo.is_local() {
        ResolvedHook::local(hook.clone())
    } else {
        let cached = cache::clone_or_reuse(project_root, repo, false).await?;
        let upstream = find_upstream(&cached.clone_dir, hook, &repo.repo, repo.rev.as_deref())?;
        let merged = crate::hooks::config::merge_with_upstream(hook, &upstream)?;
        ResolvedHook::cached(merged, cached.clone_dir)
    };

    dispatch(&resolved, staged, repo_root, executor).await
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

async fn dispatch(
    resolved: &ResolvedHook,
    staged: &[String],
    repo_root: &Path,
    executor: &Executor,
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
    // own files come from the cache). Only meaningful for host-side
    // execution — in-container hooks resolve paths against the
    // container's filesystem, not the host's cache.
    if matches!(resolved.spec.language, HookLanguage::Script)
        && resolved.spec.service.is_none()
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
            "no staged files match; skipping hook"
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

    let exit_code = run_argv(&resolved.spec, &argv, repo_root, executor).await?;
    Ok(HookOutcome {
        hook_id: resolved.spec.id.clone(),
        native: true,
        skipped: None,
        exit_code: Some(exit_code),
    })
}

/// Per-hook dispatch: explicit `in = "<service>"` wins; otherwise the
/// executor's workspace target picks host vs devcontainer. The actual
/// routing lives on [`Executor::hook_exec`] so the four spawn
/// surfaces (service / devcontainer / host inherit / host capture)
/// stay in one place.
async fn run_argv(
    spec: &HookSpec,
    argv: &[String],
    repo_root: &Path,
    executor: &Executor,
) -> Result<i32, HookError> {
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    executor
        .hook_exec(spec.service.as_deref(), &argv_refs, repo_root)
        .await
        .map_err(|e| HookError::Runtime(Box::new(e)))
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
            service: None,
        }
    }

    fn host_executor(project_root: &Path) -> Executor {
        use crate::config::Config;
        use crate::container::null::NullBackend;
        use std::sync::Arc;
        Executor::new(
            Arc::new(NullBackend) as Arc<dyn crate::container::Backend>,
            Arc::new(Config::default()),
            project_root,
        )
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
                    service: None,
                }],
            }],
            default_stages: vec![],
        };
        let executor = host_executor(temp.path());
        let err = run_pre_commit(&cfg, temp.path(), "pre-commit", &executor)
            .await
            .unwrap_err();
        assert!(matches!(err, HookError::MetaRepoNotSupported));
    }

    #[tokio::test]
    async fn local_python_hook_runs_verbatim_when_entry_on_path() {
        // The language tag is advisory now — keel doesn't gate on it.
        // Use a host binary that always exists so the spawn succeeds.
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
                    entry: Some("true".into()),
                    args: vec![],
                    files: None,
                    exclude: None,
                    pass_filenames: false,
                    stages: vec![],
                    always_run: true,
                    service: None,
                }],
            }],
            default_stages: vec![],
        };
        let executor = host_executor(temp.path());
        let outcomes = run_pre_commit(&cfg, temp.path(), "pre-commit", &executor)
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].exit_code, Some(0));
        assert!(outcomes[0].passed());
    }

    #[tokio::test]
    async fn hook_with_explicit_service_routes_through_backend() {
        use crate::config::Config;
        use crate::container::{Backend, BackendError, ExecOptions, ServiceStatus};
        use async_trait::async_trait;
        use std::sync::{Arc, Mutex};

        struct RecordingBackend {
            calls: Mutex<Vec<(String, Vec<String>)>>,
        }
        #[async_trait]
        impl Backend for RecordingBackend {
            fn name(&self) -> &'static str {
                "rec"
            }
            async fn status(&self, _service: &str) -> Result<ServiceStatus, BackendError> {
                Ok(ServiceStatus::Running)
            }
            async fn exec(
                &self,
                service: &str,
                argv: &[&str],
                _opts: &ExecOptions,
            ) -> Result<i32, BackendError> {
                self.calls.lock().unwrap().push((
                    service.to_string(),
                    argv.iter().map(|s| (*s).to_string()).collect(),
                ));
                Ok(0)
            }
            async fn passthrough(&self, _args: &[&str]) -> Result<i32, BackendError> {
                Ok(0)
            }
            async fn exec_with_stdin(
                &self,
                _service: &str,
                _argv: &[&str],
                _opts: &ExecOptions,
                _stdin: &str,
            ) -> Result<i32, BackendError> {
                Ok(0)
            }
        }

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
                    id: "fmt".into(),
                    name: None,
                    language: HookLanguage::System,
                    entry: Some("cargo fmt --check".into()),
                    args: vec![],
                    files: None,
                    exclude: None,
                    pass_filenames: false,
                    stages: vec![],
                    always_run: true,
                    service: Some("app".into()),
                }],
            }],
            default_stages: vec![],
        };
        let backend = Arc::new(RecordingBackend {
            calls: Mutex::new(Vec::new()),
        });
        let executor = Executor::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            Arc::new(Config::default()),
            temp.path(),
        );
        let outcomes = run_pre_commit(&cfg, temp.path(), "pre-commit", &executor)
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].exit_code, Some(0));
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "app");
        assert_eq!(calls[0].1, vec!["cargo", "fmt", "--check"]);
    }
}
