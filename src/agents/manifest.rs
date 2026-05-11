//! Upstream `keel-agents.toml` manifest types.
//!
//! Each upstream repo declares which of its files map to which paths
//! in the downstream project, plus an optional `mode` toggle that
//! relaxes keel's normal whole-file ownership for seed files.

use crate::agents::error::AgentsError;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Wire form of `keel-agents.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamManifest {
    #[serde(default, rename = "file")]
    pub files: Vec<FileMapping>,

    #[serde(default, rename = "dir")]
    pub dirs: Vec<DirMapping>,
}

/// One source file → one destination file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileMapping {
    pub src: String,
    pub dest: String,
    #[serde(default)]
    pub mode: FileMode,
}

/// A whole source directory expanded into individual `dest/<rel>`
/// entries at apply time. The `glob` filters which files participate
/// (default `**/*`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirMapping {
    pub src: String,
    pub dest: String,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub mode: FileMode,
}

/// Per-mapping write strategy. Default is [`FileMode::Replace`] —
/// strict whole-file ownership.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// Always overwrite from upstream. Drift triggers a warning (or an
    /// overwrite with `--force-overwrite-drift`).
    #[default]
    Replace,
    /// Write only if the destination is absent. Never overwrite,
    /// never warn on drift — the project owns the file after first
    /// install.
    Once,
}

/// Parse a manifest from raw bytes. The `path` is captured purely for
/// error reporting.
pub fn parse_manifest(path: &Path, raw: &str) -> Result<UpstreamManifest, AgentsError> {
    let manifest: UpstreamManifest =
        toml::from_str(raw).map_err(|e| AgentsError::ManifestParse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
    validate(&manifest, path)?;
    Ok(manifest)
}

fn validate(manifest: &UpstreamManifest, path: &Path) -> Result<(), AgentsError> {
    for f in &manifest.files {
        validate_rel(&f.src, "src", path)?;
        validate_rel(&f.dest, "dest", path)?;
    }
    for d in &manifest.dirs {
        validate_rel(&d.src, "src", path)?;
        validate_rel(&d.dest, "dest", path)?;
        if let Some(glob) = &d.glob {
            globset::Glob::new(glob).map_err(|e| AgentsError::ManifestInvalid {
                path: path.to_path_buf(),
                message: format!("invalid glob `{glob}`: {e}"),
            })?;
        }
    }
    Ok(())
}

/// Reject paths that escape the project root or are absolute.
fn validate_rel(value: &str, field: &'static str, manifest: &Path) -> Result<(), AgentsError> {
    let p = PathBuf::from(value);
    if p.is_absolute() {
        return Err(AgentsError::ManifestInvalid {
            path: manifest.to_path_buf(),
            message: format!("`{field}` must be a relative path, got `{value}`"),
        });
    }
    for c in p.components() {
        if matches!(c, Component::ParentDir) {
            return Err(AgentsError::ManifestInvalid {
                path: manifest.to_path_buf(),
                message: format!("`{field}` may not contain `..` segments (got `{value}`)"),
            });
        }
    }
    if value.is_empty() {
        return Err(AgentsError::ManifestInvalid {
            path: manifest.to_path_buf(),
            message: format!("`{field}` must not be empty"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_minimal_manifest() {
        let raw = r#"
            [[file]]
            src  = "agents/CLAUDE.md"
            dest = "CLAUDE.md"

            [[dir]]
            src  = "skills/"
            dest = ".claude/skills/"
        "#;
        let m = parse_manifest(Path::new("keel-agents.toml"), raw).unwrap();
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.dirs.len(), 1);
        assert_eq!(m.files[0].mode, FileMode::Replace);
    }

    #[test]
    fn parses_once_mode() {
        let raw = r#"
            [[file]]
            src  = "AGENTS.md"
            dest = "AGENTS.md"
            mode = "once"
        "#;
        let m = parse_manifest(Path::new("x"), raw).unwrap();
        assert_eq!(m.files[0].mode, FileMode::Once);
    }

    #[test]
    fn rejects_absolute_paths() {
        let raw = r#"
            [[file]]
            src  = "/etc/passwd"
            dest = "x"
        "#;
        let err = parse_manifest(Path::new("x"), raw).unwrap_err();
        assert!(matches!(err, AgentsError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_parent_segments() {
        let raw = r#"
            [[file]]
            src  = "a"
            dest = "../escape"
        "#;
        let err = parse_manifest(Path::new("x"), raw).unwrap_err();
        assert!(matches!(err, AgentsError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_invalid_glob() {
        let raw = r#"
            [[dir]]
            src  = "a"
            dest = "b"
            glob = "[unclosed"
        "#;
        let err = parse_manifest(Path::new("x"), raw).unwrap_err();
        assert!(matches!(err, AgentsError::ManifestInvalid { .. }));
    }
}
