//! Worktree identity and offset computation.
//!
//! scaffl gives each git checkout (main or `git worktree add` linked)
//! a deterministic identity:
//!
//! - **slug**: a normalised string derived from the branch name (or
//!   worktree dir, or short SHA — see [`BaseRef`]).
//! - **offset**: an integer in `0..modulus`, either pinned via
//!   `[worktrees.assign]` in scaffl.toml or computed by hashing
//!   `<seed>|<slug>` with FNV-1a.
//!
//! Recipes reference these via the env vars [`Env::resolve`] injects:
//! `SCAFFL_WORKTREE_SLUG`, `SCAFFL_WORKTREE_OFFSET`, and (when
//! isolation is on) `COMPOSE_PROJECT_NAME`. Pure functions
//! ([`slugify`], [`fnv1a_32`], [`offset_for`]) are exposed so they're
//! unit-testable without git.
//!
//! [`Env::resolve`]: crate::Env::resolve

use scaffl_config::Config;
use std::collections::BTreeMap;
use std::path::Path;
use tokio::process::Command;
use tracing::trace;

/// Where the slug ultimately came from. Useful for `worktree status`
/// output and for hint messages when something's misconfigured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRef {
    /// Active git branch (`git rev-parse --abbrev-ref HEAD`).
    Branch(String),
    /// Detached HEAD in the main checkout — slug is `det-<short sha>`.
    DetachedSha(String),
    /// Linked git worktree without a usable branch (rare; uses dir basename).
    WorktreeDir(String),
    /// Not a git repo at all. Slug is empty, offset 0, no isolation.
    None,
}

impl BaseRef {
    pub fn label(&self) -> &str {
        match self {
            BaseRef::Branch(b) => b.as_str(),
            BaseRef::DetachedSha(s) => s.as_str(),
            BaseRef::WorktreeDir(d) => d.as_str(),
            BaseRef::None => "<not a git repo>",
        }
    }
}

/// Resolved worktree identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub slug: String,
    pub base_ref: BaseRef,
    pub offset: u32,
}

impl Identity {
    /// Empty / no-isolation identity (offset 0, empty slug).
    pub fn none() -> Self {
        Self {
            slug: String::new(),
            base_ref: BaseRef::None,
            offset: 0,
        }
    }

    pub fn is_isolated(&self) -> bool {
        !self.slug.is_empty()
    }

    /// Detect the identity for the project rooted at `project_root`.
    ///
    /// Order of fallback:
    /// 1. Active branch — `BaseRef::Branch`.
    /// 2. Detached HEAD with a SHA — `BaseRef::DetachedSha`.
    /// 3. Linked worktree without a branch — `BaseRef::WorktreeDir`.
    /// 4. Not a git repo — `BaseRef::None`.
    ///
    /// Pure helpers handle the offset; this function is async only
    /// because it shells out to `git`. Detection failures (bad SHA,
    /// non-utf8 paths) collapse to `BaseRef::None` rather than
    /// erroring out — the TUI / CLI must work in non-git directories.
    pub async fn detect(project_root: &Path, config: &Config) -> Self {
        let base_ref = detect_base_ref(project_root).await;
        let slug = match &base_ref {
            BaseRef::Branch(name) => slugify(name),
            BaseRef::DetachedSha(sha) => format!("det-{sha}"),
            BaseRef::WorktreeDir(dir) => slugify(dir),
            BaseRef::None => String::new(),
        };
        let offset = offset_for(
            &slug,
            &config.worktrees.assign,
            config.worktrees.modulus,
            config.resolved_seed(),
        );
        trace!(slug = %slug, offset, ?base_ref, "worktree identity detected");
        Self {
            slug,
            base_ref,
            offset,
        }
    }
}

async fn detect_base_ref(project_root: &Path) -> BaseRef {
    // 1. Try the branch first. Returns "HEAD" on detached.
    if let Some(branch) = git_output(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .filter(|b| b != "HEAD" && !b.is_empty())
    {
        return BaseRef::Branch(branch);
    }

    // 2. Detached HEAD or no-branch — see if we're in a linked worktree.
    if let Some(git_dir) = git_output(project_root, &["rev-parse", "--git-dir"]).await
        && git_dir.contains("/worktrees/")
        && let Some(toplevel) = git_output(project_root, &["rev-parse", "--show-toplevel"]).await
        && let Some(name) = Path::new(&toplevel)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    {
        return BaseRef::WorktreeDir(name);
    }

    // 3. Detached HEAD in main checkout — fall back to short SHA.
    if let Some(sha) = git_output(project_root, &["rev-parse", "--short", "HEAD"])
        .await
        .filter(|s| !s.is_empty())
    {
        return BaseRef::DetachedSha(sha);
    }

    // 4. Not a git repo (or git is missing).
    BaseRef::None
}

/// Resolve the trunk branch the diff view should use as its
/// merge-base anchor.
///
/// Order:
/// 1. Caller-supplied `override_branch` (from `[diff] base = ...`
///    in `scaffl.toml`).
/// 2. `git symbolic-ref refs/remotes/origin/HEAD` — the canonical
///    "remote default branch" answer when a remote is configured.
/// 3. Local-branch fallback: `main`, `master`, `develop`, `trunk`,
///    in that order. Returns the first one that exists.
///
/// Returns `None` when nothing matches — caller (the TUI) treats
/// that as "no trunk found" and degrades to `git diff HEAD`.
pub async fn detect_trunk(project_root: &Path, override_branch: Option<&str>) -> Option<String> {
    if let Some(b) = override_branch
        && !b.is_empty()
    {
        return Some(b.to_string());
    }
    if let Some(out) = git_output(project_root, &["symbolic-ref", "refs/remotes/origin/HEAD"]).await
        && let Some(rest) = out.strip_prefix("refs/remotes/origin/")
    {
        return Some(rest.to_string());
    }
    for candidate in ["main", "master", "develop", "trunk"] {
        if git_output(
            project_root,
            &["rev-parse", "--verify", "--quiet", candidate],
        )
        .await
        .is_some()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Compute the merge-base SHA between `trunk` and HEAD. None on
/// failure (no shared history, trunk doesn't exist locally, etc.).
pub async fn merge_base(project_root: &Path, trunk: &str) -> Option<String> {
    git_output(project_root, &["merge-base", trunk, "HEAD"]).await
}

/// One row in the branch picker for the worktree-create form. Bare
/// short names — what `git worktree add` accepts directly. When
/// `remote_only` is true, no local branch of this name exists yet;
/// passing the name to `git worktree add` makes git auto-create a
/// local tracking branch off the matching remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    pub name: String,
    pub remote_only: bool,
}

/// List branches a user might want to base a new worktree on.
///
/// Returns local branches plus remote branches that have no local
/// counterpart, sorted by committer date descending so the most
/// recently-touched branch comes first. Symbolic refs like
/// `origin/HEAD` are skipped.
///
/// Empty Vec on any git failure — the caller (the create form)
/// degrades to "type a branch name freehand" rather than blocking.
pub async fn list_branches(project_root: &Path) -> Vec<BranchEntry> {
    use std::collections::HashSet;

    // Tab separator — branch names are git-validated and can't
    // contain whitespace, so this is robust without escaping.
    let format = "--format=%(refname:short)\t%(committerdate:unix)";

    let local_raw = git_output(
        project_root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            format,
            "refs/heads",
        ],
    )
    .await
    .unwrap_or_default();
    let remote_raw = git_output(
        project_root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            format,
            "refs/remotes",
        ],
    )
    .await
    .unwrap_or_default();

    let mut entries: Vec<(String, u64, bool)> = Vec::new();
    let mut local_names: HashSet<String> = HashSet::new();

    for line in local_raw.lines() {
        let Some((name, date)) = line.split_once('\t') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let date: u64 = date.parse().unwrap_or(0);
        local_names.insert(name.to_string());
        entries.push((name.to_string(), date, false));
    }

    for line in remote_raw.lines() {
        let Some((short, date)) = line.split_once('\t') else {
            continue;
        };
        // Skip the symbolic origin/HEAD pointer — it's not a real
        // branch users would create a worktree from.
        if short.ends_with("/HEAD") {
            continue;
        }
        // Strip the remote prefix (`origin/feature` → `feature`).
        // Branch names CAN contain slashes (`feature/auth`), so we
        // only strip the *first* segment.
        let Some(slash) = short.find('/') else {
            continue;
        };
        let bare = &short[slash + 1..];
        if bare.is_empty() || local_names.contains(bare) {
            continue;
        }
        let date: u64 = date.parse().unwrap_or(0);
        entries.push((bare.to_string(), date, true));
    }

    // Re-sort merged list by date so a recent remote-only branch
    // can rank above older local ones.
    entries.sort_by_key(|b| std::cmp::Reverse(b.1));
    entries
        .into_iter()
        .map(|(name, _, remote_only)| BranchEntry { name, remote_only })
        .collect()
}

/// Run `git <args>` in `project_root`. Returns trimmed stdout if the
/// command succeeded. Any failure (non-zero, missing git, bad utf-8)
/// collapses to `None`.
async fn git_output(project_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    Some(s.trim().to_string())
}

/// Compute an offset for a slug given the project's pin map and hash
/// settings.
///
/// Empty slug → 0 (special-case: no isolation). Pinned slugs win over
/// the hash. Otherwise: `fnv1a_32("<seed>|<slug>") % max(modulus, 1)`.
pub fn offset_for(slug: &str, assign: &BTreeMap<String, u32>, modulus: u32, seed: &str) -> u32 {
    if slug.is_empty() {
        return 0;
    }
    if let Some(&pinned) = assign.get(slug) {
        return pinned;
    }
    let combined = format!("{seed}|{slug}");
    fnv1a_32(&combined) % modulus.max(1)
}

/// 32-bit FNV-1a, hand-rolled. Stable across Rust versions, machines,
/// and re-compilations (unlike `std::collections::hash_map::DefaultHasher`).
pub fn fnv1a_32(input: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in input.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Convert a string to a stable slug for use in env vars and overlay
/// file paths.
///
/// Rules: lowercase ASCII; replace any non-`[a-z0-9-]` character with
/// `-`; collapse runs of `-`; trim leading/trailing dashes.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true; // suppresses leading dashes
    for c in input.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            out.push(lc);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// One worktree from `git worktree list --porcelain`.
///
/// `branch` is normalised to the short form (`feature/x`, not
/// `refs/heads/feature/x`); `None` means a detached HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub path: String,
    pub branch: Option<String>,
    pub detached: bool,
}

/// List worktrees by shelling out to `git worktree list --porcelain`.
/// Returns an empty Vec on any failure (not a git repo, git missing,
/// non-utf8 output) — callers treat "no worktrees" identically to
/// "no git", which is the right UX for the worktree switcher.
pub async fn list_worktrees(project_root: &Path) -> Vec<WorktreeListEntry> {
    let Ok(output) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    parse_worktree_porcelain(&stdout)
}

/// Parse `git worktree list --porcelain` output. One blank line
/// separates each worktree's stanza. Public for tests and reuse.
pub fn parse_worktree_porcelain(input: &str) -> Vec<WorktreeListEntry> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut detached = false;
    for line in input.lines() {
        if line.is_empty() {
            if let Some(p) = path.take() {
                out.push(WorktreeListEntry {
                    path: p,
                    branch: branch.take(),
                    detached: std::mem::take(&mut detached),
                });
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            // `branch refs/heads/feature/x` → strip the prefix.
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        } else if line == "detached" {
            detached = true;
        }
    }
    if let Some(p) = path {
        out.push(WorktreeListEntry {
            path: p,
            branch,
            detached,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_porcelain_minimal_two_worktrees() {
        let input = "\
worktree /home/me/proj
HEAD abcd1234
branch refs/heads/main

worktree /home/me/proj-feature
HEAD ef567890
branch refs/heads/feature/x
";
        let entries = parse_worktree_porcelain(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "/home/me/proj");
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(!entries[0].detached);
        assert_eq!(entries[1].branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn parse_porcelain_detached_entry() {
        let input = "\
worktree /home/me/proj-detached
HEAD abcd1234
detached
";
        let entries = parse_worktree_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].detached);
        assert!(entries[0].branch.is_none());
    }

    #[test]
    fn parse_porcelain_handles_trailing_no_blank_line() {
        // Real-world git output sometimes lacks a trailing blank line
        // on the last stanza. Make sure we still emit the final entry.
        let input = "worktree /a\nHEAD aaa\nbranch refs/heads/main";
        let entries = parse_worktree_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/a");
    }

    #[tokio::test]
    async fn detect_trunk_override_wins_short_circuit() {
        // Override-set short-circuits before any git invocation, so
        // a nonsense path still yields the override. Confirms the
        // priority order documented on `detect_trunk`.
        let path = std::path::Path::new("/nonexistent-scaffl-test-path");
        let result = detect_trunk(path, Some("release/stable")).await;
        assert_eq!(result.as_deref(), Some("release/stable"));
    }

    #[tokio::test]
    async fn detect_trunk_empty_override_falls_through() {
        // Empty string override is treated as "unset" — not a valid
        // branch name, and forcing `Some("")` would short-circuit
        // detection where users probably meant None.
        let path = std::path::Path::new("/nonexistent-scaffl-test-path");
        let result = detect_trunk(path, Some("")).await;
        // Falls through to git lookups, which fail on nonexistent
        // path → None.
        assert_eq!(result, None);
    }

    #[test]
    fn slugify_lowercases_and_dashifies() {
        assert_eq!(slugify("main"), "main");
        assert_eq!(slugify("feature/x"), "feature-x");
        assert_eq!(slugify("Refactor PR/2"), "refactor-pr-2");
        assert_eq!(slugify("---multi---dashes---"), "multi-dashes");
        assert_eq!(slugify("  spaces  "), "spaces");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("Feature/X-1"), "feature-x-1");
    }

    #[test]
    fn fnv1a_known_vectors() {
        // Reference values from http://www.isthe.com/chongo/tech/comp/fnv/
        assert_eq!(fnv1a_32(""), 0x811c_9dc5);
        assert_eq!(fnv1a_32("a"), 0xe40c_292c);
        assert_eq!(fnv1a_32("foobar"), 0xbf9c_f968);
    }

    #[test]
    fn fnv1a_is_deterministic() {
        assert_eq!(fnv1a_32("scaffl|feature-x"), fnv1a_32("scaffl|feature-x"));
        assert_ne!(fnv1a_32("scaffl|feature-x"), fnv1a_32("scaffl|feature-y"));
    }

    #[test]
    fn empty_slug_offset_is_zero() {
        let assign = BTreeMap::new();
        assert_eq!(offset_for("", &assign, 1000, "seed"), 0);
    }

    #[test]
    fn pinned_offset_wins_over_hash() {
        let mut assign = BTreeMap::new();
        assign.insert("main".to_string(), 0);
        assign.insert("feature-x".to_string(), 7);
        assert_eq!(offset_for("main", &assign, 1000, "seed"), 0);
        assert_eq!(offset_for("feature-x", &assign, 1000, "seed"), 7);
    }

    #[test]
    fn hashed_offset_is_within_modulus() {
        let assign = BTreeMap::new();
        for slug in ["a", "feature-x", "really-long-branch-name-here"] {
            let offset = offset_for(slug, &assign, 100, "seed");
            assert!(offset < 100, "offset {offset} not < 100 for slug {slug}");
        }
    }

    #[test]
    fn hashed_offset_diverges_with_seed() {
        let assign = BTreeMap::new();
        let a = offset_for("feature-x", &assign, 1000, "project-a");
        let b = offset_for("feature-x", &assign, 1000, "project-b");
        assert_ne!(a, b, "different seeds should produce different offsets");
    }

    #[test]
    fn modulus_zero_does_not_panic() {
        let assign = BTreeMap::new();
        // Treated as 1; result will be 0.
        let off = offset_for("anything", &assign, 0, "seed");
        assert_eq!(off, 0);
    }

    #[test]
    fn identity_none_is_unisolated() {
        let id = Identity::none();
        assert!(!id.is_isolated());
        assert_eq!(id.offset, 0);
        assert!(id.slug.is_empty());
    }
}
