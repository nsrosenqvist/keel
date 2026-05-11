//! Merge upstream manifests with downstream overrides + expand
//! `[[dir]]` mappings against the cloned worktree. Produces a flat
//! list of `ResolvedEntry` ready for the apply pipeline.

use crate::agents::error::AgentsError;
use crate::agents::manifest::{FileMode, UpstreamManifest};
use crate::config::{ResolvedOverride, SourceSpec};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One concrete source-file → destination-file mapping after manifest
/// expansion and downstream override application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    /// Owning source's `name` (for collision reports + state).
    pub source_name: String,
    /// Absolute path inside the cached clone.
    pub src_abs: PathBuf,
    /// Source path *relative to the clone root + subpath*. Recorded
    /// in state for diff display and orphan tracking.
    pub src_rel: String,
    /// Destination path relative to the project root.
    pub dest: PathBuf,
    pub mode: FileMode,
}

/// Expand a single source's manifest into resolved entries with
/// overrides applied. The `clone_root` is the directory returned by
/// `keel-cache`; `subpath` (if any) is joined to it before any src
/// is resolved.
pub fn expand_source(
    source: &SourceSpec,
    clone_root: &Path,
    manifest: &UpstreamManifest,
) -> Result<Vec<ResolvedEntry>, AgentsError> {
    let root = match &source.subpath {
        Some(s) if !s.is_empty() => clone_root.join(s),
        _ => clone_root.to_path_buf(),
    };
    let overrides = build_override_index(source)?;

    let mut entries = Vec::new();

    for file in &manifest.files {
        let dest_key = PathBuf::from(&file.dest);
        match overrides.get(&dest_key) {
            Some(ResolvedOverride::Skip) => continue,
            Some(ResolvedOverride::Relocate(new_dest)) => {
                entries.push(ResolvedEntry {
                    source_name: source.name.clone(),
                    src_abs: root.join(&file.src),
                    src_rel: file.src.clone(),
                    dest: PathBuf::from(new_dest),
                    mode: file.mode,
                });
            }
            None => {
                entries.push(ResolvedEntry {
                    source_name: source.name.clone(),
                    src_abs: root.join(&file.src),
                    src_rel: file.src.clone(),
                    dest: dest_key,
                    mode: file.mode,
                });
            }
        }
    }

    for dir in &manifest.dirs {
        let dir_src_root = root.join(&dir.src);
        let dest_dir_root = PathBuf::from(&dir.dest);

        if !dir_src_root.is_dir() {
            // An upstream that declares a dir mapping for a missing
            // directory is a manifest bug — fail loudly rather than
            // silently dropping the mapping.
            return Err(AgentsError::ManifestInvalid {
                path: dir_src_root.clone(),
                message: format!(
                    "source `{}`: dir mapping src `{}` not found inside upstream clone",
                    source.name, dir.src
                ),
            });
        }

        let matcher = match &dir.glob {
            Some(g) => Some(
                globset::Glob::new(g).map_err(|e| AgentsError::ManifestInvalid {
                    path: dir_src_root.clone(),
                    message: format!("invalid glob `{g}`: {e}"),
                })?,
            ),
            None => None,
        }
        .map(|g| g.compile_matcher());

        for entry in walkdir::WalkDir::new(&dir_src_root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let rel = entry.path().strip_prefix(&dir_src_root).map_err(|_| {
                AgentsError::ManifestInvalid {
                    path: entry.path().to_path_buf(),
                    message: "walkdir produced a file outside the src root".to_string(),
                }
            })?;

            if let Some(m) = &matcher
                && !m.is_match(rel)
            {
                continue;
            }

            let dest = dest_dir_root.join(rel);
            let src_rel = join_str(&dir.src, &rel.to_string_lossy());

            match overrides.get(&dest) {
                Some(ResolvedOverride::Skip) => continue,
                Some(ResolvedOverride::Relocate(new_dest)) => {
                    entries.push(ResolvedEntry {
                        source_name: source.name.clone(),
                        src_abs: entry.path().to_path_buf(),
                        src_rel,
                        dest: PathBuf::from(new_dest),
                        mode: dir.mode,
                    });
                }
                None => {
                    entries.push(ResolvedEntry {
                        source_name: source.name.clone(),
                        src_abs: entry.path().to_path_buf(),
                        src_rel,
                        dest,
                        mode: dir.mode,
                    });
                }
            }
        }
    }

    Ok(entries)
}

fn build_override_index(
    source: &SourceSpec,
) -> Result<BTreeMap<PathBuf, ResolvedOverride>, AgentsError> {
    let mut out = BTreeMap::new();
    for ov in &source.overrides {
        let resolved = ov.resolved().map_err(AgentsError::Config)?;
        out.insert(PathBuf::from(&ov.dest), resolved);
    }
    Ok(out)
}

fn join_str(prefix: &str, rel: &str) -> String {
    if prefix.is_empty() {
        return rel.to_string();
    }
    let trimmed = prefix.trim_end_matches('/');
    format!("{trimmed}/{rel}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::manifest::{DirMapping, FileMapping, UpstreamManifest};
    use crate::config::MappingOverride;
    use tempfile::TempDir;

    fn dummy_source(name: &str, overrides: Vec<MappingOverride>) -> SourceSpec {
        SourceSpec {
            name: name.into(),
            repo: "x".into(),
            rev: "v1".into(),
            subpath: None,
            manifest_path: None,
            overrides,
        }
    }

    #[test]
    fn expands_file_mappings_as_is() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "hi").unwrap();
        let manifest = UpstreamManifest {
            files: vec![FileMapping {
                src: "CLAUDE.md".into(),
                dest: "CLAUDE.md".into(),
                mode: FileMode::Replace,
            }],
            dirs: vec![],
        };
        let source = dummy_source("baseline", vec![]);
        let entries = expand_source(&source, dir.path(), &manifest).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dest, PathBuf::from("CLAUDE.md"));
    }

    #[test]
    fn skip_override_drops_mapping() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        let manifest = UpstreamManifest {
            files: vec![FileMapping {
                src: "AGENTS.md".into(),
                dest: "AGENTS.md".into(),
                mode: FileMode::Replace,
            }],
            dirs: vec![],
        };
        let source = dummy_source(
            "baseline",
            vec![MappingOverride {
                dest: "AGENTS.md".into(),
                action: Some(crate::config::MappingOverrideKind::Skip),
                relocate: None,
            }],
        );
        let entries = expand_source(&source, dir.path(), &manifest).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn relocate_override_rewrites_dest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a"), "x").unwrap();
        let manifest = UpstreamManifest {
            files: vec![FileMapping {
                src: "a".into(),
                dest: "b".into(),
                mode: FileMode::Replace,
            }],
            dirs: vec![],
        };
        let source = dummy_source(
            "x",
            vec![MappingOverride {
                dest: "b".into(),
                action: None,
                relocate: Some("c".into()),
            }],
        );
        let entries = expand_source(&source, dir.path(), &manifest).unwrap();
        assert_eq!(entries[0].dest, PathBuf::from("c"));
    }

    #[test]
    fn dir_mapping_expands_each_file() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(skills.join("nested")).unwrap();
        std::fs::write(skills.join("a.md"), "a").unwrap();
        std::fs::write(skills.join("b.md"), "b").unwrap();
        std::fs::write(skills.join("nested/c.md"), "c").unwrap();
        std::fs::write(skills.join("ignore.txt"), "").unwrap();

        let manifest = UpstreamManifest {
            files: vec![],
            dirs: vec![DirMapping {
                src: "skills".into(),
                dest: ".claude/skills".into(),
                glob: Some("**/*.md".into()),
                mode: FileMode::Replace,
            }],
        };
        let source = dummy_source("baseline", vec![]);
        let mut entries = expand_source(&source, dir.path(), &manifest).unwrap();
        entries.sort_by(|a, b| a.dest.cmp(&b.dest));
        let dests: Vec<_> = entries.iter().map(|e| e.dest.clone()).collect();
        assert_eq!(
            dests,
            vec![
                PathBuf::from(".claude/skills/a.md"),
                PathBuf::from(".claude/skills/b.md"),
                PathBuf::from(".claude/skills/nested/c.md"),
            ]
        );
    }

    #[test]
    fn subpath_is_applied_before_resolution() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("claude");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "x").unwrap();

        let manifest = UpstreamManifest {
            files: vec![FileMapping {
                src: "CLAUDE.md".into(),
                dest: "CLAUDE.md".into(),
                mode: FileMode::Replace,
            }],
            dirs: vec![],
        };
        let mut source = dummy_source("baseline", vec![]);
        source.subpath = Some("claude".into());
        let entries = expand_source(&source, dir.path(), &manifest).unwrap();
        assert_eq!(entries[0].src_abs, sub.join("CLAUDE.md"));
    }

    #[test]
    fn missing_dir_src_is_a_manifest_error() {
        let dir = TempDir::new().unwrap();
        let manifest = UpstreamManifest {
            files: vec![],
            dirs: vec![DirMapping {
                src: "nope".into(),
                dest: "out".into(),
                glob: None,
                mode: FileMode::Replace,
            }],
        };
        let source = dummy_source("baseline", vec![]);
        let err = expand_source(&source, dir.path(), &manifest).unwrap_err();
        assert!(matches!(err, AgentsError::ManifestInvalid { .. }));
    }
}
