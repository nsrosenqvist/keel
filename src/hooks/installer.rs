//! Git hook installer.
//!
//! Writes shim scripts under `.git/hooks/<stage>` that delegate to
//! `ampelos hooks run <stage> "$@"`. Each shim contains a marker comment so
//! we can tell ours apart from a hand-written one and refuse to overwrite
//! anything we didn't author.

use crate::hooks::error::HookError;
use crate::hooks::git;
use std::path::{Path, PathBuf};

pub const AMPELOS_HOOK_MARKER: &str = "# managed by ampelos";

/// Stages we know how to install. Other stages can be supported by
/// passing them explicitly to `install` — this list only governs which
/// stages `uninstall` cleans up by default.
pub const KNOWN_STAGES: &[&str] = &[
    "pre-commit",
    "pre-push",
    "commit-msg",
    "post-commit",
    "pre-merge-commit",
    "pre-rebase",
    // post-checkout / post-merge are how the env-rewrite flow keeps
    // `.env` in sync with the active worktree's offset / slug.
    "post-checkout",
    "post-merge",
];

/// Install shims for `stages` under the git repo containing `project_root`.
/// Returns the list of hook paths that were written.
pub fn install(project_root: &Path, stages: &[&str]) -> Result<Vec<PathBuf>, HookError> {
    let repo_root = git::discover_repo(project_root)?;
    let hooks_dir = repo_root.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|source| HookError::Io {
        path: hooks_dir.clone(),
        source,
    })?;
    let mut written = Vec::with_capacity(stages.len());
    for stage in stages {
        let path = install_one(&hooks_dir, stage)?;
        written.push(path);
    }
    Ok(written)
}

/// Write a shim for a single stage. Refuses to overwrite a non-ampelos
/// hook.
pub fn install_one(hooks_dir: &Path, stage: &str) -> Result<PathBuf, HookError> {
    let path = hooks_dir.join(stage);
    if path.exists() {
        let existing = std::fs::read_to_string(&path).map_err(|source| HookError::Io {
            path: path.clone(),
            source,
        })?;
        if !existing.contains(AMPELOS_HOOK_MARKER) {
            return Err(HookError::HookExists { path });
        }
    }
    std::fs::write(&path, render_shim(stage)).map_err(|source| HookError::Io {
        path: path.clone(),
        source,
    })?;
    set_executable(&path)?;
    Ok(path)
}

/// Remove ampelos-managed shims from `stages`. Untouched: hooks we didn't
/// author (the marker check) and stages that don't have a hook installed.
pub fn uninstall(project_root: &Path, stages: &[&str]) -> Result<Vec<PathBuf>, HookError> {
    let repo_root = git::discover_repo(project_root)?;
    let hooks_dir = repo_root.join(".git").join("hooks");
    let mut removed = Vec::new();
    for stage in stages {
        let path = hooks_dir.join(stage);
        if !path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&path).map_err(|source| HookError::Io {
            path: path.clone(),
            source,
        })?;
        if !body.contains(AMPELOS_HOOK_MARKER) {
            continue;
        }
        std::fs::remove_file(&path).map_err(|source| HookError::Io {
            path: path.clone(),
            source,
        })?;
        removed.push(path);
    }
    Ok(removed)
}

fn render_shim(stage: &str) -> String {
    format!(
        "#!/bin/sh\n\
         {AMPELOS_HOOK_MARKER}\n\
         exec ampelos hooks run {stage} \"$@\"\n"
    )
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), HookError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .map_err(|source| HookError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), HookError> {
    // Windows / non-unix: rely on the file extension and the user's setup.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join(".git").join("hooks")).unwrap();
        dir
    }

    #[test]
    fn install_writes_shim() {
        let dir = fake_repo();
        let written = install(dir.path(), &["pre-commit"]).unwrap();
        assert_eq!(written.len(), 1);
        let body = std::fs::read_to_string(&written[0]).unwrap();
        assert!(body.contains(AMPELOS_HOOK_MARKER));
        assert!(body.contains("ampelos hooks run pre-commit"));
    }

    #[test]
    fn install_overwrites_own_shim() {
        let dir = fake_repo();
        install(dir.path(), &["pre-commit"]).unwrap();
        // Second install should succeed (we recognise our own marker).
        install(dir.path(), &["pre-commit"]).unwrap();
    }

    #[test]
    fn install_refuses_foreign_hook() {
        let dir = fake_repo();
        let path = dir.path().join(".git").join("hooks").join("pre-commit");
        std::fs::write(&path, "#!/bin/sh\necho hand-written\n").unwrap();
        let err = install(dir.path(), &["pre-commit"]).unwrap_err();
        assert!(matches!(err, HookError::HookExists { .. }));
        // Original untouched.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("hand-written"));
    }

    #[test]
    fn uninstall_removes_only_our_shims() {
        let dir = fake_repo();
        install(dir.path(), &["pre-commit", "pre-push"]).unwrap();
        // Add a foreign one.
        let foreign = dir.path().join(".git").join("hooks").join("commit-msg");
        std::fs::write(&foreign, "#!/bin/sh\necho mine\n").unwrap();
        let removed = uninstall(dir.path(), &["pre-commit", "pre-push", "commit-msg"]).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(foreign.exists());
    }
}
