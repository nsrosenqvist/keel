//! Pre-commit-flavoured wrapper around the `scaffl-cache` crate.
//!
//! `scaffl-cache` does the actual cloning and on-disk caching. This
//! shim translates the hooks-side `Repo` value object into a plain
//! `RepoRef` and forwards through, so callers in `scaffl-hooks` and
//! `scaffl-cli` don't need to know about the cache crate's API.

use crate::config::Repo;
use crate::error::HookError;
use scaffl_cache::{CacheError, CacheKind, RepoRef};

pub use scaffl_cache::{CacheMeta, CachedRepo};

/// Cache root for hook repos. Equivalent to
/// `<project>/.scaffl/cache/hooks/`.
pub fn cache_root(project_root: &std::path::Path) -> std::path::PathBuf {
    scaffl_cache::cache_root(project_root, CacheKind::Hooks)
}

/// Clone `repo` into the project's hook cache, or return the existing
/// entry. `force = true` wipes and re-clones (the `--update-hooks`
/// flag).
pub async fn clone_or_reuse(
    project_root: &std::path::Path,
    repo: &Repo,
    force: bool,
) -> Result<CachedRepo, HookError> {
    let rev = repo.rev.as_deref().ok_or_else(|| CacheError::MissingRev {
        repo: repo.repo.clone(),
    })?;
    Ok(scaffl_cache::clone_or_reuse(
        project_root,
        &RepoRef {
            repo: repo.repo.clone(),
            rev: rev.to_string(),
        },
        force,
        CacheKind::Hooks,
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Repo;
    use scaffl_cache::CacheError;
    use tempfile::TempDir;

    #[tokio::test]
    async fn missing_rev_surfaces_through_hookerror() {
        let project = TempDir::new().unwrap();
        let repo = Repo {
            repo: "https://example.com/foo".into(),
            rev: None,
            hooks: vec![],
        };
        let err = clone_or_reuse(project.path(), &repo, false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            HookError::Cache(CacheError::MissingRev { .. })
        ));
    }
}
