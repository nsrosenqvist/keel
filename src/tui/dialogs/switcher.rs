//! Worktree-switcher dialog state.
//!
//! `WorktreeRow` is one row in the switcher list. `NewWorktreeForm`
//! and friends describe the sub-state for the "create new worktree"
//! sub-flow. The container `WorktreeSwitcher` itself still lives on
//! [`crate::tui::app`] until Phase 4 fuses the modals.

use crate::runtime::BranchEntry;
use std::path::PathBuf;

/// One row in the worktree switcher list. Slug is computed by the
/// runtime crate; `is_current` flags the worktree keel is
/// currently bound to so we can render it differently.
#[derive(Debug, Clone)]
pub struct WorktreeRow {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub slug: String,
    pub is_current: bool,
}

/// Sub-state for the "create new worktree" flow inside the
/// switcher modal.
///
/// Shape: branch-first picker. The user types into `branch_input`,
/// which fuzzy-filters `branches` (sourced from `git for-each-ref`)
/// down to `filtered`. A "create new branch" sentinel appears as
/// the last option whenever the input doesn't exactly match an
/// existing branch — selecting it triggers `git worktree add -b`.
///
/// `path_input` stays auto-synced as `<parent>/<slug(branch)>`
/// while `path_dirty` is false. The first edit in the path field
/// flips that flag and the path stops following the branch — the
/// user owns it from then on.
#[derive(Debug, Clone)]
pub struct NewWorktreeForm {
    pub branch_input: String,
    pub path_input: String,
    /// True once the user has manually edited the path field. Once
    /// dirty, branch keystrokes no longer rewrite the path — we
    /// don't want to clobber what the user typed.
    pub path_dirty: bool,
    /// Parent directory new worktrees go under. Defaults to the
    /// current project root's parent dir; baked in at form-open
    /// time so the path-derivation code doesn't need to keep
    /// rerunning the lookup.
    pub parent: PathBuf,
    /// All branches the user might want to base a worktree on,
    /// pre-fetched once when the form opens. Sorted by committer
    /// date desc; remote-only branches included.
    pub branches: Vec<BranchEntry>,
    /// Indices into `branches`, filtered by the current branch
    /// input. Recomputed on every keystroke that mutates the
    /// branch field.
    pub filtered: Vec<usize>,
    /// Highlighted row in the [filtered ++ sentinel] list. Bound
    /// to `filtered.len()` (the sentinel slot, when present) at
    /// the upper end. The renderer/key-handler treats `selected ==
    /// filtered.len()` as "the create-new-branch sentinel."
    pub selected: usize,
    pub focus: NewFormField,
    /// Last error from `git worktree add`, if any. Surfaces as a
    /// hint inside the modal so the user can fix and retry without
    /// the modal closing.
    pub error: Option<String>,
}

impl NewWorktreeForm {
    /// True when the create-new-branch sentinel should appear as
    /// the last option. We only show it when:
    ///   - `branch_input` is non-empty (no point creating a branch
    ///     called nothing), and
    ///   - no existing branch in `branches` matches `branch_input`
    ///     exactly (avoids "create new branch 'main' off HEAD"
    ///     when `main` already exists).
    pub fn show_create_sentinel(&self) -> bool {
        if self.branch_input.is_empty() {
            return false;
        }
        !self.branches.iter().any(|b| b.name == self.branch_input)
    }

    /// Total selectable rows — filtered branches plus the sentinel
    /// when applicable. Used by the bounds-check on Up/Down.
    pub fn total_options(&self) -> usize {
        self.filtered.len() + if self.show_create_sentinel() { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewFormField {
    Path,
    Branch,
}

/// What the form's confirm step asks `git worktree add` to do.
#[derive(Debug, Clone)]
pub struct NewWorktreeAction {
    pub path: String,
    pub branch: BranchSpec,
}

/// Distinguishes "attach an existing branch" from "create a new
/// branch off HEAD." Drives whether `git worktree add` gets `-b`.
#[derive(Debug, Clone)]
pub enum BranchSpec {
    /// Use an existing branch — `git worktree add <path> <branch>`.
    /// For remote-only entries, git auto-creates a tracking branch.
    Existing(String),
    /// Create a new branch off HEAD —
    /// `git worktree add <path> -b <branch>`.
    CreateOff(String),
}

/// Outcome of [`crate::tui::app::App::switcher_confirm`]. The caller
/// (terminal layer) pattern-matches to decide whether to fetch
/// branches and reopen the modal in create-form mode, or fall
/// through to the queued hot-reload, or do nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitcherConfirm {
    /// Selected row was an existing worktree → switch queued.
    Switched,
    /// Selected row was the "+ new worktree" sentinel → fetch
    /// branches async and call `App::open_create_form`.
    OpenCreateForm,
    /// Switcher modal wasn't open → key dispatcher should ignore.
    NoOp,
}
