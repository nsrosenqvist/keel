//! `.keel/install.state.json` — what's been run, what's still pending.
//!
//! The state file is a thin record of per-step outcome plus an overall
//! `started_at`. The runner reads it to decide what `--resume` should
//! pick up from; it writes a fresh copy after every step so a Ctrl-C
//! mid-install can still be resumed. Writes are atomic (write to a
//! sibling `.tmp` file, then rename) so a crashed process never leaves
//! a half-written JSON document behind.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// Not yet attempted. Default on construction.
    Pending,
    /// Most recent run exited 0.
    Ok,
    /// Most recent run exited non-zero (and the step was not optional,
    /// or the user retried specifically).
    Failed,
    /// Optional step whose most recent run exited non-zero. Counts as
    /// "done" for `--resume` purposes — re-running install does not
    /// re-attempt it unless the user names the step explicitly.
    Skipped,
}

impl StepStatus {
    /// Whether `--resume` treats this status as "already handled" and
    /// skips past it.
    pub fn is_resolved(self) -> bool {
        matches!(self, Self::Ok | Self::Skipped)
    }
}

/// Per-step record. Most fields are `Option` so the file can be
/// written before each step starts (with `status = Pending` and
/// timestamps unset) and updated in place when the step finishes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepRecord {
    pub name: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// UNIX epoch milliseconds. Plain `u64` rather than RFC3339 keeps
    /// the serde surface free of `chrono` / `time` dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
}

impl StepRecord {
    pub fn pending(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Pending,
            exit_code: None,
            duration_ms: None,
            started_at_ms: None,
            ended_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallState {
    pub version: u32,
    pub started_at_ms: u64,
    pub steps: Vec<StepRecord>,
}

impl InstallState {
    /// Build a fresh state for the given plan (every step `Pending`).
    pub fn fresh(step_names: impl IntoIterator<Item = String>) -> Self {
        Self {
            version: SCHEMA_VERSION,
            started_at_ms: now_ms(),
            steps: step_names.into_iter().map(StepRecord::pending).collect(),
        }
    }

    /// Read the state file if present. Returns `Ok(None)` when the
    /// file doesn't exist; errors when it does exist but can't be
    /// parsed (corruption or schema mismatch — the user should know).
    pub fn load(project_root: &Path) -> Result<Option<Self>> {
        let path = state_path(project_root);
        if !path.exists() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: Self =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        if parsed.version != SCHEMA_VERSION {
            anyhow::bail!(
                "{} has version {}, expected {}. Remove the file or run `ampelos install --restart`.",
                path.display(),
                parsed.version,
                SCHEMA_VERSION,
            );
        }
        Ok(Some(parsed))
    }

    /// Atomic write: serialise, write to a sibling `.tmp` file, then
    /// rename over the target. Survives Ctrl-C mid-write.
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let path = state_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(self).context("serialise install state")?;
        std::fs::write(&tmp_path, raw).with_context(|| format!("write {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &path)
            .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
        Ok(())
    }

    /// Remove the state file. Used by `--restart`. Tolerates absence.
    pub fn wipe(project_root: &Path) -> Result<()> {
        let path = state_path(project_root);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }

    /// Index of the first step whose status is not yet resolved. Used
    /// to drive `--resume`.
    pub fn first_unresolved(&self) -> Option<usize> {
        self.steps.iter().position(|s| !s.status.is_resolved())
    }

    /// Locate a record by name. Used by single-step mode (`ampelos
    /// install <name>`) to update just one entry.
    pub fn find_mut(&mut self, name: &str) -> Option<&mut StepRecord> {
        self.steps.iter_mut().find(|s| s.name == name)
    }
}

fn state_path(project_root: &Path) -> PathBuf {
    project_root.join(".keel").join("install.state.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Public wrapper around the millisecond clock so the runner doesn't
/// have to import `SystemTime` and other modules can keep their
/// timestamping consistent.
pub fn epoch_ms() -> u64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fresh_state_marks_every_step_pending() {
        let state = InstallState::fresh(["a".into(), "b".into()]);
        assert_eq!(state.version, SCHEMA_VERSION);
        assert_eq!(state.steps.len(), 2);
        assert!(state.steps.iter().all(|s| s.status == StepStatus::Pending));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut state = InstallState::fresh(["a".into(), "b".into()]);
        state.steps[0].status = StepStatus::Ok;
        state.steps[0].exit_code = Some(0);
        state.steps[0].duration_ms = Some(42);
        state.save(dir.path()).unwrap();
        let loaded = InstallState::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = TempDir::new().unwrap();
        assert!(InstallState::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn wipe_removes_existing_file_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let state = InstallState::fresh(["a".into()]);
        state.save(dir.path()).unwrap();
        assert!(state_path(dir.path()).exists());
        InstallState::wipe(dir.path()).unwrap();
        assert!(!state_path(dir.path()).exists());
        // Second wipe is a no-op.
        InstallState::wipe(dir.path()).unwrap();
    }

    #[test]
    fn first_unresolved_skips_ok_and_skipped() {
        let mut state = InstallState::fresh(["a".into(), "b".into(), "c".into(), "d".into()]);
        state.steps[0].status = StepStatus::Ok;
        state.steps[1].status = StepStatus::Skipped;
        state.steps[2].status = StepStatus::Failed;
        state.steps[3].status = StepStatus::Pending;
        assert_eq!(state.first_unresolved(), Some(2));
    }

    #[test]
    fn first_unresolved_returns_none_when_all_resolved() {
        let mut state = InstallState::fresh(["a".into(), "b".into()]);
        state.steps[0].status = StepStatus::Ok;
        state.steps[1].status = StepStatus::Skipped;
        assert!(state.first_unresolved().is_none());
    }

    #[test]
    fn version_mismatch_errors() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".keel")).unwrap();
        std::fs::write(
            state_path(dir.path()),
            r#"{"version":999,"started_at_ms":0,"steps":[]}"#,
        )
        .unwrap();
        let err = InstallState::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("version 999"));
    }
}
