//! Minimal git interactions: repo discovery and staged-file listing.
//!
//! We deliberately shell out to `git` rather than pulling in `git2` for
//! these operations — installation is one syscall, the staged-file query
//! is one. The dependency cost of `git2` (libgit2, openssl, zlib) is not
//! justified for what we need.

use crate::hooks::error::HookError;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Locate the git working-tree root for `start`. Returns the absolute path
/// of the directory containing `.git/`, walking up from `start`.
pub fn discover_repo(start: &Path) -> Result<PathBuf, HookError> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Ok(cur);
        }
        if !cur.pop() {
            return Err(HookError::NotARepo(start.to_path_buf()));
        }
    }
}

/// Return staged files (added, copied, modified, renamed) relative to the
/// repo root.
pub async fn staged_files(repo_root: &Path) -> Result<Vec<String>, HookError> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "--cached", "--diff-filter=ACMR"])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|e| HookError::GitFailed(format!("spawn git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HookError::GitFailed(format!(
            "git diff exited {}: {stderr}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_repo_walks_up() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let found = discover_repo(&nested).unwrap();
        assert_eq!(found, dir.path());
    }

    #[test]
    fn discover_repo_errors_when_not_a_repo() {
        let dir = TempDir::new().unwrap();
        let err = discover_repo(dir.path()).unwrap_err();
        assert!(matches!(err, HookError::NotARepo(_)));
    }
}
