//! End-to-end apply pipeline.
//!
//! `apply()` is the single entry point used by both `scaffl agents
//! install` / `update` and the synthetic step inside `scaffl install`.
//! It orchestrates the cache, manifest loading, override merge,
//! drift / collision / shadow checks, and the actual file writes
//! plus state-file maintenance.

use crate::error::AgentsError;
use crate::manifest::{FileMode, parse_manifest};
use crate::merge::{ResolvedEntry, expand_source};
use crate::state::{AgentsState, AppliedFile, SourceRecord, epoch_ms};
use scaffl_cache::{CacheKind, RepoRef};
use scaffl_config::{AgentsConfig, SourceSpec};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Knobs for one apply invocation.
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// Re-clone every source, ignoring the cache.
    pub force: bool,
    /// Plan but don't write files or save state.
    pub dry_run: bool,
    /// Overwrite scaffl-owned files that have been hand-edited since
    /// the last apply. Off by default — drifted files are left alone
    /// and listed in the report.
    pub force_overwrite_drift: bool,
    /// When non-empty, only sources whose `name` appears in this list
    /// participate. Other sources are left at their previously-applied
    /// state (no orphan removal for those).
    pub source_filter: Option<Vec<String>>,
}

/// Per-source resolved revision + manifest fingerprint. Persisted to
/// state and surfaced by `agents status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceResult {
    pub name: String,
    pub repo: String,
    pub rev_request: String,
    pub resolved_sha: String,
    pub manifest_sha256: String,
}

/// One destination edited by hand since scaffl last wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftEntry {
    pub dest: PathBuf,
    pub recorded_sha: String,
    pub disk_sha: String,
}

/// Two or more sources resolved the same destination. Later-declared
/// source wins; the report includes the loser names so the user can
/// add an `overrides` entry if needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestCollision {
    pub dest: PathBuf,
    pub winning_source: String,
    pub overshadowed_sources: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub written: Vec<PathBuf>,
    pub updated: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    /// `mode = "once"` entries skipped because the file already exists.
    pub once_kept: Vec<PathBuf>,
    pub drift_warnings: Vec<DriftEntry>,
    pub collisions: Vec<DestCollision>,
    pub sources: Vec<SourceResult>,
    pub dry_run: bool,
}

/// Whether the rev is a floating ref we should auto-refetch on
/// every apply.
pub fn is_floating_rev(rev: &str) -> bool {
    if rev.is_empty() {
        return true;
    }
    if rev.len() >= 7 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let after_v = rev.strip_prefix('v').unwrap_or(rev);
    if !after_v.is_empty()
        && after_v.starts_with(|c: char| c.is_ascii_digit())
        && after_v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && after_v.contains('.')
    {
        return false;
    }
    true
}

/// Main entry point. Borrowed `Path` so callers can pass any
/// directory; the function itself never changes the process CWD.
pub async fn apply(
    project_root: &Path,
    config: &AgentsConfig,
    opts: &ApplyOptions,
) -> Result<ApplyReport, AgentsError> {
    // 1. Load state.
    let prior_state = AgentsState::load(project_root)?.unwrap_or_default();

    // 2. Drift scan against prior state.
    let drift = detect_drift(&prior_state, project_root)?;
    let drifted: BTreeSet<PathBuf> = drift.iter().map(|d| d.dest.clone()).collect();

    // 3. For each (filtered) source: clone, parse manifest, expand.
    let mut resolved: BTreeMap<PathBuf, ResolvedEntry> = BTreeMap::new();
    let mut collision_groups: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut source_results: Vec<SourceResult> = Vec::new();

    let active_sources: Vec<&SourceSpec> = config
        .sources
        .iter()
        .filter(|s| match &opts.source_filter {
            Some(filter) => filter.iter().any(|f| f == &s.name),
            None => true,
        })
        .collect();

    for source in &active_sources {
        let force = opts.force || is_floating_rev(&source.rev);
        let cached = scaffl_cache::clone_or_reuse(
            project_root,
            &RepoRef {
                repo: source.repo.clone(),
                rev: source.rev.clone(),
            },
            force,
            CacheKind::Agents,
        )
        .await?;

        let manifest_rel = source
            .manifest_path
            .as_deref()
            .unwrap_or(&config.manifest_path);
        let manifest_path = match &source.subpath {
            Some(p) if !p.is_empty() => cached.clone_dir.join(p).join(manifest_rel),
            _ => cached.clone_dir.join(manifest_rel),
        };

        let manifest_raw =
            std::fs::read_to_string(&manifest_path).map_err(|source| AgentsError::Io {
                path: manifest_path.clone(),
                source,
            })?;
        let manifest_sha = sha256_hex(manifest_raw.as_bytes());
        let manifest = parse_manifest(&manifest_path, &manifest_raw)?;

        let entries = expand_source(source, &cached.clone_dir, &manifest)?;
        for entry in entries {
            if let Some(prev) = resolved.insert(entry.dest.clone(), entry.clone()) {
                let group = collision_groups.entry(entry.dest.clone()).or_default();
                if !group.contains(&prev.source_name) {
                    group.push(prev.source_name);
                }
                group.push(entry.source_name.clone());
            }
        }

        source_results.push(SourceResult {
            name: source.name.clone(),
            repo: source.repo.clone(),
            rev_request: source.rev.clone(),
            resolved_sha: cached.resolved_sha,
            manifest_sha256: manifest_sha,
        });
    }

    // 4. Build a friendlier collision view (winner = last entry kept
    //    in `resolved`; losers = preceding entries in declaration order).
    let collisions: Vec<DestCollision> = collision_groups
        .into_iter()
        .map(|(dest, sources)| {
            let winner = resolved
                .get(&dest)
                .map(|e| e.source_name.clone())
                .unwrap_or_default();
            let losers: Vec<String> = sources.into_iter().filter(|s| s != &winner).collect();
            DestCollision {
                dest,
                winning_source: winner,
                overshadowed_sources: losers,
            }
        })
        .collect();

    // 5. Local-sibling shadow check for dir-mode targets. A file at
    //    the destination that isn't tracked in state and isn't being
    //    written by us is a shadow conflict.
    let state_dests: BTreeSet<PathBuf> = prior_state.files.iter().map(|f| f.dest.clone()).collect();
    let target_dirs: BTreeSet<PathBuf> = resolved
        .values()
        .filter_map(|e| e.dest.parent().map(Path::to_path_buf))
        .collect();
    for dir in &target_dirs {
        let abs_dir = project_root.join(dir);
        if !abs_dir.is_dir() {
            continue;
        }
        // Only enforce for dirs we expand into (i.e. containing
        // resolved entries) — otherwise a writable parent dir of a
        // single-file mapping (e.g. project root) would trip this up.
        let any_in_dir = resolved.values().any(|e| e.dest.parent() == Some(dir));
        if !any_in_dir {
            continue;
        }
        let entries = std::fs::read_dir(&abs_dir).map_err(|source| AgentsError::Io {
            path: abs_dir.clone(),
            source,
        })?;
        for ent in entries {
            let ent = ent.map_err(|source| AgentsError::Io {
                path: abs_dir.clone(),
                source,
            })?;
            if !ent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let dest_rel = dir.join(ent.file_name());
            if state_dests.contains(&dest_rel) {
                continue; // We own this one.
            }
            if !resolved.contains_key(&dest_rel) {
                continue; // Not something we'd write.
            }
            // We would write `dest_rel`, the file already exists, and
            // we don't own it. Refuse.
            let suggested = format!(
                "{}.local{}",
                dest_rel
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                dest_rel
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default(),
            );
            let entry = resolved.get(&dest_rel).expect("checked containment");
            return Err(AgentsError::LocalShadow {
                source_name: entry.source_name.clone(),
                dest: dest_rel,
                suggested,
            });
        }
    }

    // 6. Compute actions. The previous state is consumed by the
    //    "what's still left = orphan" pass at the end.
    let mut prior_files: BTreeMap<PathBuf, AppliedFile> = prior_state
        .files
        .iter()
        .cloned()
        .map(|f| (f.dest.clone(), f))
        .collect();
    // When source_filter is active, only orphans owned by filtered
    // sources are eligible for removal.
    let active_source_names: BTreeSet<String> =
        active_sources.iter().map(|s| s.name.clone()).collect();
    let filter_active = opts.source_filter.is_some();

    let mut report = ApplyReport {
        dry_run: opts.dry_run,
        sources: source_results.clone(),
        collisions,
        ..Default::default()
    };

    let mut new_files: Vec<AppliedFile> = Vec::new();
    let now = epoch_ms();

    for (dest, entry) in &resolved {
        let abs_dest = project_root.join(dest);
        let prior = prior_files.remove(dest);

        match (entry.mode, prior) {
            (FileMode::Once, Some(prev)) => {
                // We already let go; never re-write.
                new_files.push(AppliedFile {
                    dest: dest.clone(),
                    source_name: entry.source_name.clone(),
                    src: entry.src_rel.clone(),
                    sha256: None,
                    mode: FileMode::Once,
                    written_at_ms: prev.written_at_ms,
                });
                report.once_kept.push(dest.clone());
            }
            (FileMode::Once, None) => {
                if abs_dest.exists() {
                    // Someone else owns it (could be a fresh project
                    // that already had AGENTS.md). Track it so orphan
                    // detection still works, but don't overwrite.
                    new_files.push(AppliedFile {
                        dest: dest.clone(),
                        source_name: entry.source_name.clone(),
                        src: entry.src_rel.clone(),
                        sha256: None,
                        mode: FileMode::Once,
                        written_at_ms: now,
                    });
                    report.once_kept.push(dest.clone());
                } else {
                    let bytes = read_src(&entry.src_abs)?;
                    if !opts.dry_run {
                        write_file(&abs_dest, &bytes)?;
                    }
                    new_files.push(AppliedFile {
                        dest: dest.clone(),
                        source_name: entry.source_name.clone(),
                        src: entry.src_rel.clone(),
                        sha256: None,
                        mode: FileMode::Once,
                        written_at_ms: now,
                    });
                    report.written.push(dest.clone());
                }
            }
            (FileMode::Replace, prev) => {
                let upstream_bytes = read_src(&entry.src_abs)?;
                let upstream_sha = sha256_hex(&upstream_bytes);

                // Drift: prior state recorded a sha, disk hash differs.
                if drifted.contains(dest) && !opts.force_overwrite_drift {
                    // Leave drift alone but keep ownership.
                    if let Some(p) = &prev {
                        new_files.push(AppliedFile {
                            dest: dest.clone(),
                            source_name: entry.source_name.clone(),
                            src: entry.src_rel.clone(),
                            sha256: p.sha256.clone(),
                            mode: FileMode::Replace,
                            written_at_ms: p.written_at_ms,
                        });
                    }
                    continue;
                }

                let needs_write = if drifted.contains(dest) {
                    // We got past the leave-alone branch above only
                    // when --force-overwrite-drift is set; reach
                    // here means "user wants disk replaced".
                    true
                } else {
                    match (&prev, abs_dest.exists()) {
                        (Some(p), true) => p.sha256.as_deref() != Some(&upstream_sha),
                        (_, false) => true,
                        (None, true) => {
                            // Fresh take-over. Whole-file ownership ≈ overwrite.
                            let disk_sha = sha256_hex(&read_src(&abs_dest)?);
                            disk_sha != upstream_sha
                        }
                    }
                };

                if needs_write {
                    if !opts.dry_run {
                        write_file(&abs_dest, &upstream_bytes)?;
                    }
                    if prev.is_some() {
                        report.updated.push(dest.clone());
                    } else {
                        report.written.push(dest.clone());
                    }
                } else {
                    report.unchanged.push(dest.clone());
                }
                new_files.push(AppliedFile {
                    dest: dest.clone(),
                    source_name: entry.source_name.clone(),
                    src: entry.src_rel.clone(),
                    sha256: Some(upstream_sha),
                    mode: FileMode::Replace,
                    written_at_ms: now,
                });
            }
        }
    }

    // 7. Orphans: anything still in `prior_files` belongs to a source
    //    that no longer claims it. Remove (or pass through if filtered).
    for (dest, prev) in prior_files {
        if filter_active && !active_source_names.contains(&prev.source_name) {
            // Filtered run — keep the entry for an unrelated source.
            new_files.push(prev);
            continue;
        }
        let abs_dest = project_root.join(&dest);
        if abs_dest.exists()
            && !opts.dry_run
            && let Err(source) = std::fs::remove_file(&abs_dest)
        {
            return Err(AgentsError::Io {
                path: abs_dest.clone(),
                source,
            });
        }
        if !opts.dry_run {
            prune_empty_parents(project_root, &dest);
        }
        report.removed.push(dest);
    }

    report.drift_warnings = drift;

    // 8. Persist state.
    if !opts.dry_run {
        let state = AgentsState {
            version: prior_state.version.max(1),
            applied_at_ms: now,
            sources: source_results
                .iter()
                .map(|r| SourceRecord {
                    name: r.name.clone(),
                    repo: r.repo.clone(),
                    rev_request: r.rev_request.clone(),
                    resolved_sha: r.resolved_sha.clone(),
                    manifest_sha256: r.manifest_sha256.clone(),
                })
                .collect(),
            files: {
                new_files.sort_by(|a, b| a.dest.cmp(&b.dest));
                new_files
            },
        };
        state.save(project_root)?;
    }

    Ok(report)
}

/// Drift = scaffl-owned file whose disk content hashes to something
/// other than what we last wrote. Once-mode entries are excluded
/// (sha256 is `None`), and missing files are not drift either (the
/// apply pass will silently re-create them).
pub fn detect_drift(
    state: &AgentsState,
    project_root: &Path,
) -> Result<Vec<DriftEntry>, AgentsError> {
    let mut out = Vec::new();
    for file in &state.files {
        let Some(expected) = file.sha256.as_deref() else {
            continue;
        };
        let abs = project_root.join(&file.dest);
        if !abs.exists() {
            continue;
        }
        let disk = read_src(&abs)?;
        let disk_sha = sha256_hex(&disk);
        if disk_sha != expected {
            out.push(DriftEntry {
                dest: file.dest.clone(),
                recorded_sha: expected.to_string(),
                disk_sha,
            });
        }
    }
    Ok(out)
}

fn read_src(path: &Path) -> Result<Vec<u8>, AgentsError> {
    std::fs::read(path).map_err(|source| AgentsError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), AgentsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AgentsError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = path.with_extension(format!(
        "{}.scaffl-tmp",
        path.extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ));
    std::fs::write(&tmp, bytes).map_err(|source| AgentsError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| AgentsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Remove empty parent directories under the project root, walking
/// upward from the removed file. Stops at the project root.
fn prune_empty_parents(project_root: &Path, dest_rel: &Path) {
    let mut cursor = dest_rel.parent();
    while let Some(parent_rel) = cursor {
        if parent_rel.as_os_str().is_empty() {
            break;
        }
        let abs = project_root.join(parent_rel);
        let is_empty = std::fs::read_dir(&abs)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if !is_empty {
            break;
        }
        if std::fs::remove_dir(&abs).is_err() {
            break;
        }
        cursor = parent_rel.parent();
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut out = String::with_capacity(result.len() * 2);
    for byte in result {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_rev_detection() {
        assert!(!is_floating_rev("abcdef1234567"));
        assert!(!is_floating_rev("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_floating_rev("v1.4.0"));
        assert!(!is_floating_rev("1.4.0"));
        assert!(!is_floating_rev("v2.0.0-rc.1"));

        assert!(is_floating_rev("main"));
        assert!(is_floating_rev("master"));
        assert!(is_floating_rev("develop"));
        assert!(is_floating_rev("HEAD"));
        assert!(is_floating_rev(""));
        // Bare 'v1' without a dot — too ambiguous to treat as a tag.
        assert!(is_floating_rev("v1"));
    }

    #[test]
    fn sha256_hex_is_deterministic_and_64_chars() {
        let a = sha256_hex(b"hello");
        let b = sha256_hex(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(a, sha256_hex(b"world"));
    }
}
