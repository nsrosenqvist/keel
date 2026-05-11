//! `.keel/agents.state.json` — what keel owns, what it pinned to,
//! what bytes it last wrote. Drives drift detection and orphan
//! removal across `agents install` / `agents update` runs.
//!
//! Atomic write via temp-file-and-rename, mirroring the install
//! state in `keel-cli`.

use crate::agents::error::AgentsError;
use crate::agents::manifest::FileMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SCHEMA_VERSION: u32 = 1;
const STATE_FILE: &str = "agents.state.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsState {
    pub version: u32,
    pub applied_at_ms: u64,
    #[serde(default)]
    pub sources: Vec<SourceRecord>,
    #[serde(default)]
    pub files: Vec<AppliedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRecord {
    pub name: String,
    pub repo: String,
    pub rev_request: String,
    pub resolved_sha: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedFile {
    pub dest: PathBuf,
    pub source_name: String,
    pub src: String,
    /// SHA-256 of the bytes keel last wrote. `None` for `mode =
    /// once` (we explicitly let go after first write, so the drift
    /// scan must skip the dest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub mode: FileMode,
    pub written_at_ms: u64,
}

impl AgentsState {
    pub fn fresh() -> Self {
        Self {
            version: SCHEMA_VERSION,
            applied_at_ms: epoch_ms(),
            sources: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Load the state file if present. Returns `Ok(None)` when absent.
    /// Errors on corruption or schema mismatch.
    pub fn load(project_root: &Path) -> Result<Option<Self>, AgentsError> {
        let path = state_path(project_root);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| AgentsError::Io {
            path: path.clone(),
            source,
        })?;
        let parsed: Self = serde_json::from_str(&raw).map_err(|e| AgentsError::Io {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
        })?;
        if parsed.version != SCHEMA_VERSION {
            return Err(AgentsError::Io {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "agents.state.json has version {}, expected {}. Remove the file and re-run `keel agents install`.",
                        parsed.version, SCHEMA_VERSION
                    ),
                ),
            });
        }
        Ok(Some(parsed))
    }

    /// Atomic write.
    pub fn save(&self, project_root: &Path) -> Result<(), AgentsError> {
        let path = state_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AgentsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(self).map_err(|e| AgentsError::Io {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        std::fs::write(&tmp_path, raw).map_err(|source| AgentsError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &path).map_err(|source| AgentsError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Locate an applied file by destination path.
    pub fn find(&self, dest: &Path) -> Option<&AppliedFile> {
        self.files.iter().find(|f| f.dest == dest)
    }

    /// Path of the state file (public for tests + the gitignore writer).
    pub fn path(project_root: &Path) -> PathBuf {
        state_path(project_root)
    }
}

fn state_path(project_root: &Path) -> PathBuf {
    project_root.join(".keel").join(STATE_FILE)
}

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_state() -> AgentsState {
        AgentsState {
            version: SCHEMA_VERSION,
            applied_at_ms: 1,
            sources: vec![SourceRecord {
                name: "baseline".into(),
                repo: "https://example/x.git".into(),
                rev_request: "v1".into(),
                resolved_sha: "deadbeef".into(),
                manifest_sha256: "cafe".into(),
            }],
            files: vec![AppliedFile {
                dest: PathBuf::from("CLAUDE.md"),
                source_name: "baseline".into(),
                src: "agents/CLAUDE.md".into(),
                sha256: Some("abc".into()),
                mode: FileMode::Replace,
                written_at_ms: 1,
            }],
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let s = sample_state();
        s.save(dir.path()).unwrap();
        let loaded = AgentsState::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = TempDir::new().unwrap();
        assert!(AgentsState::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn version_mismatch_is_an_error() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".keel")).unwrap();
        std::fs::write(
            state_path(dir.path()),
            r#"{"version":999,"applied_at_ms":0,"sources":[],"files":[]}"#,
        )
        .unwrap();
        let err = AgentsState::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("version 999"));
    }

    #[test]
    fn once_mode_serialises_without_sha() {
        let mut s = sample_state();
        s.files[0].sha256 = None;
        s.files[0].mode = FileMode::Once;
        let raw = serde_json::to_string(&s).unwrap();
        // The file record drops `sha256`; `manifest_sha256` on the
        // source record still appears, so check the file slice.
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let file = &parsed["files"][0];
        assert!(file.get("sha256").is_none(), "got {file}");
    }
}
