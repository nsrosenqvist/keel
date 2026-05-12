//! Content-addressed git cache for ampelos.
//!
//! ampelos owns the cache — it never delegates to a third-party tool.
//! Every cache lives at `<project>/.keel/cache/<kind>/<key>/`, one
//! directory per (url, rev) pair, where `<kind>` is selected by the
//! caller (hooks, agents, …). Cache contents survive across runs;
//! callers pass `force = true` to wipe and re-clone.
//!
//! Layout per cached repo:
//!
//! ```text
//! .keel/cache/<kind>/<key>/
//!   ├── clone/            // git worktree (depth-1 when possible)
//!   └── meta.json         // resolved SHA + clone metadata
//! ```
//!
//! Cache keys are derived from (url, rev) using a deterministic slug
//! that's safe as a filesystem path segment.

use crate::cache::error::CacheError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const CLONE_SUBDIR: &str = "clone";
const META_FILE: &str = "meta.json";

/// Logical cache namespace. Selects the on-disk directory under
/// `.keel/cache/`. New variants only need to map to a path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    Hooks,
    Agents,
}

impl CacheKind {
    fn as_segment(&self) -> &'static str {
        match self {
            CacheKind::Hooks => "hooks",
            CacheKind::Agents => "agents",
        }
    }
}

/// Minimal input for a cache lookup. Callers translate their own
/// repo/source types into this on the way in so `ampelos-cache` does
/// not depend on any of them.
#[derive(Debug, Clone)]
pub struct RepoRef {
    pub repo: String,
    pub rev: String,
}

/// Resolved cache entry for an external repo.
#[derive(Debug, Clone)]
pub struct CachedRepo {
    /// Directory holding the cloned worktree (the `<key>/clone` path).
    pub clone_dir: PathBuf,
    /// SHA the cached clone points at. May equal the input `rev` when
    /// it was already a full sha; otherwise the result of
    /// `git rev-parse HEAD` against the cloned worktree.
    pub resolved_sha: String,
}

/// Persisted alongside each cached repo; lets callers report what SHA
/// `rev = v4.5.0` ended up at without re-running git.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheMeta {
    pub repo: String,
    pub rev: String,
    pub resolved_sha: String,
}

/// Return the cache root for a given project + kind. Created on demand
/// by [`clone_or_reuse`]; callers don't need to pre-create it.
pub fn cache_root(project_root: &Path, kind: CacheKind) -> PathBuf {
    project_root
        .join(".keel")
        .join("cache")
        .join(kind.as_segment())
}

/// Clone `repo` into the project's cache, or return the existing entry
/// if it's already cached. `force` deletes and re-clones.
pub async fn clone_or_reuse(
    project_root: &Path,
    repo: &RepoRef,
    force: bool,
    kind: CacheKind,
) -> Result<CachedRepo, CacheError> {
    if repo.rev.is_empty() {
        return Err(CacheError::MissingRev {
            repo: repo.repo.clone(),
        });
    }

    let key = cache_key(&repo.repo, &repo.rev);
    let entry_dir = cache_root(project_root, kind).join(&key);
    let clone_dir = entry_dir.join(CLONE_SUBDIR);
    let meta_path = entry_dir.join(META_FILE);

    if force && entry_dir.exists() {
        std::fs::remove_dir_all(&entry_dir).map_err(|source| CacheError::Io {
            path: entry_dir.clone(),
            source,
        })?;
    }

    if clone_dir.is_dir() && meta_path.is_file() {
        let meta_raw = std::fs::read_to_string(&meta_path).map_err(|source| CacheError::Io {
            path: meta_path.clone(),
            source,
        })?;
        if let Ok(meta) = serde_json::from_str::<CacheMeta>(&meta_raw) {
            return Ok(CachedRepo {
                clone_dir,
                resolved_sha: meta.resolved_sha,
            });
        }
        // Meta corrupted but clone exists — fall through to re-clone.
        std::fs::remove_dir_all(&entry_dir).map_err(|source| CacheError::Io {
            path: entry_dir.clone(),
            source,
        })?;
    }

    std::fs::create_dir_all(&entry_dir).map_err(|source| CacheError::Io {
        path: entry_dir.clone(),
        source,
    })?;

    run_clone(&repo.repo, &repo.rev, &clone_dir).await?;
    let resolved_sha = run_rev_parse(&clone_dir).await?;

    let meta = CacheMeta {
        repo: repo.repo.clone(),
        rev: repo.rev.clone(),
        resolved_sha: resolved_sha.clone(),
    };
    let meta_raw = serde_json::to_string_pretty(&meta).expect("CacheMeta serialises");
    std::fs::write(&meta_path, meta_raw).map_err(|source| CacheError::Io {
        path: meta_path,
        source,
    })?;

    Ok(CachedRepo {
        clone_dir,
        resolved_sha,
    })
}

async fn run_clone(repo: &str, rev: &str, dest: &Path) -> Result<(), CacheError> {
    // `--depth 1 --branch <rev>` resolves both tags and branches. For a
    // raw SHA this fails on most servers; we retry with a full clone +
    // detached checkout in that case.
    let depth_attempt = Command::new("git")
        .args(["clone", "--depth", "1", "--branch", rev])
        .arg(repo)
        .arg(dest)
        .output()
        .await
        .map_err(|e| CacheError::CloneFailed {
            repo: repo.to_string(),
            rev: rev.to_string(),
            message: format!("spawn git: {e}"),
        })?;

    if depth_attempt.status.success() {
        return Ok(());
    }

    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }

    let full_clone = Command::new("git")
        .args(["clone"])
        .arg(repo)
        .arg(dest)
        .output()
        .await
        .map_err(|e| CacheError::CloneFailed {
            repo: repo.to_string(),
            rev: rev.to_string(),
            message: format!("spawn git (fallback): {e}"),
        })?;
    if !full_clone.status.success() {
        let stderr = String::from_utf8_lossy(&full_clone.stderr)
            .trim()
            .to_string();
        return Err(CacheError::CloneFailed {
            repo: repo.to_string(),
            rev: rev.to_string(),
            message: stderr,
        });
    }

    let checkout = Command::new("git")
        .args(["checkout", "--detach", rev])
        .current_dir(dest)
        .output()
        .await
        .map_err(|e| CacheError::CloneFailed {
            repo: repo.to_string(),
            rev: rev.to_string(),
            message: format!("spawn git checkout: {e}"),
        })?;
    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr).trim().to_string();
        return Err(CacheError::CloneFailed {
            repo: repo.to_string(),
            rev: rev.to_string(),
            message: stderr,
        });
    }
    Ok(())
}

async fn run_rev_parse(repo_dir: &Path) -> Result<String, CacheError> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .await
        .map_err(|e| CacheError::GitFailed(format!("rev-parse: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CacheError::GitFailed(format!("rev-parse: {stderr}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Map (url, rev) → a path-safe slug. Two repos with the same (url,
/// rev) collapse to the same key — that is the cache identity.
fn cache_key(repo: &str, rev: &str) -> String {
    fn slug(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut last_underscore = false;
        for ch in s.chars() {
            let safe = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-');
            if safe {
                out.push(ch);
                last_underscore = false;
            } else if !last_underscore {
                out.push('_');
                last_underscore = true;
            }
        }
        out.trim_matches('_').to_string()
    }
    let mut key = format!("{}-{}", slug(repo), slug(rev));
    // Filesystem-safe bound — most filesystems cap path segments at
    // 255 bytes; leave room for `/clone` and any future suffixes.
    if key.len() > 200 {
        key.truncate(200);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::process::Command as TokioCommand;

    async fn make_fixture_repo(name: &str) -> TempDir {
        let dir = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        TokioCommand::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        TokioCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        TokioCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        std::fs::write(dir.path().join("README"), "seed").unwrap();
        TokioCommand::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        TokioCommand::new("git")
            .args(["commit", "-q", "-m", "seed"])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        TokioCommand::new("git")
            .args(["tag", "v0.1.0"])
            .current_dir(dir.path())
            .status()
            .await
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn clone_then_reuse_returns_cached_entry() {
        let upstream = make_fixture_repo("upstream").await;
        let project = TempDir::new().unwrap();
        let repo = RepoRef {
            repo: upstream.path().to_string_lossy().to_string(),
            rev: "v0.1.0".into(),
        };

        let first = clone_or_reuse(project.path(), &repo, false, CacheKind::Hooks)
            .await
            .unwrap();
        assert!(first.clone_dir.is_dir());
        assert_eq!(first.resolved_sha.len(), 40);
        let first_sha = first.resolved_sha.clone();

        let second = clone_or_reuse(project.path(), &repo, false, CacheKind::Hooks)
            .await
            .unwrap();
        assert_eq!(second.resolved_sha, first_sha);
        assert_eq!(second.clone_dir, first.clone_dir);
    }

    #[tokio::test]
    async fn force_reclones_existing_entry() {
        let upstream = make_fixture_repo("upstream-force").await;
        let project = TempDir::new().unwrap();
        let repo = RepoRef {
            repo: upstream.path().to_string_lossy().to_string(),
            rev: "v0.1.0".into(),
        };

        let first = clone_or_reuse(project.path(), &repo, false, CacheKind::Hooks)
            .await
            .unwrap();
        let marker = first.clone_dir.join("FORCE_MARKER");
        std::fs::write(&marker, "x").unwrap();
        assert!(marker.exists());

        let _ = clone_or_reuse(project.path(), &repo, true, CacheKind::Hooks)
            .await
            .unwrap();
        assert!(!marker.exists(), "force should have wiped the cache entry");
    }

    #[tokio::test]
    async fn missing_rev_is_an_error() {
        let project = TempDir::new().unwrap();
        let repo = RepoRef {
            repo: "https://example.com/foo".into(),
            rev: String::new(),
        };
        let err = clone_or_reuse(project.path(), &repo, false, CacheKind::Hooks)
            .await
            .unwrap_err();
        assert!(matches!(err, CacheError::MissingRev { .. }));
    }

    #[tokio::test]
    async fn agents_kind_isolates_from_hooks_kind() {
        let upstream = make_fixture_repo("upstream-agents").await;
        let project = TempDir::new().unwrap();
        let repo = RepoRef {
            repo: upstream.path().to_string_lossy().to_string(),
            rev: "v0.1.0".into(),
        };
        let hooks_entry = clone_or_reuse(project.path(), &repo, false, CacheKind::Hooks)
            .await
            .unwrap();
        let agents_entry = clone_or_reuse(project.path(), &repo, false, CacheKind::Agents)
            .await
            .unwrap();
        assert_ne!(hooks_entry.clone_dir, agents_entry.clone_dir);
        assert!(
            agents_entry
                .clone_dir
                .to_string_lossy()
                .contains("/cache/agents/")
        );
    }

    #[test]
    fn cache_key_is_deterministic_and_path_safe() {
        let a = cache_key("https://github.com/foo/bar", "v1.0");
        let b = cache_key("https://github.com/foo/bar", "v1.0");
        assert_eq!(a, b);
        assert!(!a.contains('/'));
        assert!(!a.contains(':'));
        assert!(!a.contains(' '));
    }

    #[test]
    fn cache_key_caps_length() {
        let big = "x".repeat(500);
        let key = cache_key(&big, "v1");
        assert!(key.len() <= 200);
    }
}
