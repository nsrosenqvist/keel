//! Worktree-switcher dialog state.
//!
//! `WorktreeRow` is one row in the switcher list. `NewWorktreeForm`
//! and friends describe the sub-state for the "create new worktree"
//! sub-flow. The container `WorktreeSwitcher` itself still lives on
//! [`crate::tui::app`] until Phase 4 fuses the modals.

use crate::runtime::BranchEntry;
use ratatui::layout::Rect;
use std::cell::RefCell;
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

    /// Append `c` to the focused field. Branch keystrokes refilter
    /// the visible branch list and auto-sync the path field (until
    /// the user manually edits the path). Path keystrokes flip
    /// `path_dirty` so subsequent branch typing leaves the path
    /// alone. Clears any prior `error` so retries don't carry stale
    /// "branch already exists" messages.
    pub fn push_char(&mut self, c: char) {
        match self.focus {
            NewFormField::Path => {
                self.path_input.push(c);
                self.path_dirty = true;
            }
            NewFormField::Branch => {
                self.branch_input.push(c);
                self.refilter_branches();
                self.sync_path_from_branch();
            }
        }
        self.error = None;
    }

    pub fn pop_char(&mut self) {
        match self.focus {
            NewFormField::Path => {
                self.path_input.pop();
                self.path_dirty = true;
            }
            NewFormField::Branch => {
                self.branch_input.pop();
                self.refilter_branches();
                self.sync_path_from_branch();
            }
        }
        self.error = None;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            NewFormField::Path => NewFormField::Branch,
            NewFormField::Branch => NewFormField::Path,
        };
    }

    /// Move the highlighted branch / sentinel down. Only meaningful
    /// when focus is on the branch field (the path field has no
    /// list to navigate).
    pub fn select_next(&mut self) {
        if self.focus != NewFormField::Branch {
            return;
        }
        let total = self.total_options();
        if total > 0 {
            self.selected = (self.selected + 1).min(total - 1);
        }
    }

    pub fn select_prev(&mut self) {
        if self.focus != NewFormField::Branch {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    /// What the caller should ask `git worktree add` to do. Folds
    /// the focus + selection + sentinel logic into a single tuple
    /// the terminal layer can shell out from.
    pub fn resolve(&self) -> Option<NewWorktreeAction> {
        if self.path_input.trim().is_empty() {
            return None;
        }
        let branch = match self.focus {
            // In path-focus mode the branch list isn't relevant —
            // submit with whatever was last typed in the branch field.
            NewFormField::Path => {
                if self.branch_input.trim().is_empty() {
                    return None;
                }
                if self.branches.iter().any(|b| b.name == self.branch_input) {
                    BranchSpec::Existing(self.branch_input.clone())
                } else {
                    BranchSpec::CreateOff(self.branch_input.clone())
                }
            }
            NewFormField::Branch => {
                if self.selected < self.filtered.len() {
                    let idx = self.filtered[self.selected];
                    BranchSpec::Existing(self.branches[idx].name.clone())
                } else if self.show_create_sentinel() {
                    BranchSpec::CreateOff(self.branch_input.clone())
                } else {
                    return None;
                }
            }
        };
        Some(NewWorktreeAction {
            path: self.path_input.clone(),
            branch,
        })
    }

    /// Recompute `filtered` from the current `branch_input`. Empty
    /// query → every branch in original order; non-empty → case-
    /// insensitive substring match (good enough for v1; can swap in
    /// nucleo-matcher later if anyone asks for fuzzy ordering).
    fn refilter_branches(&mut self) {
        if self.branch_input.is_empty() {
            self.filtered = (0..self.branches.len()).collect();
        } else {
            let q = self.branch_input.to_lowercase();
            self.filtered = self
                .branches
                .iter()
                .enumerate()
                .filter(|(_, b)| b.name.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
        // Selection clamps to the new bounds. If the user types until
        // the list shrinks to just the sentinel, that's selected.
        let total = self.total_options();
        self.selected = if total == 0 {
            0
        } else {
            self.selected.min(total - 1)
        };
    }

    /// Auto-fill the path field as `<parent>/<slug(branch)>` whenever
    /// the user is typing in the branch field AND hasn't manually
    /// edited the path yet. Once `path_dirty` is true, we leave the
    /// path alone — the user owns it.
    fn sync_path_from_branch(&mut self) {
        if self.path_dirty {
            return;
        }
        let dir = if self.branch_input.is_empty() {
            String::new()
        } else {
            crate::runtime::slugify(&self.branch_input)
        };
        if dir.is_empty() {
            self.path_input.clear();
        } else {
            self.path_input = self.parent.join(dir).display().to_string();
        }
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

/// Whole switcher state. The list always has a sentinel "+ new
/// worktree" row at the end; selecting it opens `creating`.
#[derive(Debug, Clone)]
pub struct WorktreeSwitcher {
    pub entries: Vec<WorktreeRow>,
    pub selected: usize,
    pub creating: Option<NewWorktreeForm>,
    /// Per-row rects for the entries list (including the trailing
    /// "+ new worktree" sentinel). Populated by the renderer; hit-
    /// tested by the mouse handler to route clicks to a row index.
    pub row_rects: RefCell<Vec<Rect>>,
}

impl WorktreeSwitcher {
    /// Index of the synthetic "+ new worktree" row — always last.
    pub fn new_row_index(&self) -> usize {
        self.entries.len()
    }

    /// Total rows including the new-worktree sentinel.
    pub fn total_rows(&self) -> usize {
        self.entries.len() + 1
    }

    pub fn select_next(&mut self) {
        let total = self.total_rows();
        if total > 0 {
            self.selected = (self.selected + 1).min(total - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Set the selection to `idx`, clamped to the last row (which is
    /// the synthetic "+ new worktree" sentinel).
    pub fn select_at(&mut self, idx: usize) {
        let total = self.total_rows();
        if total > 0 {
            self.selected = idx.min(total - 1);
        }
    }
}

