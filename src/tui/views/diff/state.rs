//! Leaf state types for the diff view.
//!
//! The container [`crate::tui::app::DiffState`] (with its
//! caches, scroll maps, viewport cells) still lives in `app.rs`
//! until Phase 3; only the value types it composes have moved here.

use crate::tui::syntax::HighlightedSpan;

/// Which pane in the diff view has the keyboard focus. Files-pane
/// is the default — discoverable, mirrors the implicit selection
/// users already had in v1. Body-pane focus enables line-by-line
/// scroll, hunk navigation, and gg/G.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffFocus {
    #[default]
    Files,
    Body,
}

/// What the diff view's right-hand pane is currently showing.
/// `Diff` is the default unified-diff view; `Read` shows the full
/// file (working-tree contents for present files, `git show
/// <anchor>:<path>` for deleted files). Global across files —
/// toggling switches both panes' currently-selected file
/// simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyMode {
    #[default]
    Diff,
    Read,
}

#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub status: DiffStatus,
    /// Lines added / removed against the anchor. `(0, 0)` until
    /// numstat lands; `(0, 0)` permanently for binary files (we set
    /// `binary = true` so the file list can render `bin` instead of
    /// `+0 −0`).
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
    /// For renames (R rows in `--name-status`): the path the file
    /// had at the anchor. Threaded into `load_diff_for_file` so the
    /// per-file diff command can resolve the rename and report
    /// similarity + actual content delta instead of "new file at
    /// <new_path>, +N lines".
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Other,
}

impl DiffStatus {
    pub fn letter(self) -> char {
        match self {
            DiffStatus::Modified => 'M',
            DiffStatus::Added => 'A',
            DiffStatus::Deleted => 'D',
            DiffStatus::Renamed => 'R',
            DiffStatus::Untracked => 'U',
            DiffStatus::Other => '?',
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    /// Old-side line number. None on Added / Hunk / Header lines.
    pub old_lineno: Option<u32>,
    /// New-side line number. None on Removed / Hunk / Header lines.
    pub new_lineno: Option<u32>,
    /// Pre-computed syntect spans for the inner code text (after
    /// stripping the leading `+`/`-`/` ` sigil). Empty for hunk
    /// and header lines.
    pub spans: Vec<HighlightedSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Added,
    Removed,
    Context,
    Hunk,
    Header,
}

impl DiffLineKind {
    pub fn classify(line: &str) -> Self {
        if line.starts_with("@@") {
            return DiffLineKind::Hunk;
        }
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("Binary files")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
            || line.starts_with("similarity index")
            || line.starts_with("rename from")
            || line.starts_with("rename to")
            || line.starts_with("copy from")
            || line.starts_with("copy to")
        {
            return DiffLineKind::Header;
        }
        if line.starts_with('+') {
            return DiffLineKind::Added;
        }
        if line.starts_with('-') {
            return DiffLineKind::Removed;
        }
        DiffLineKind::Context
    }
}

/// One line of a file rendered in read mode: no diff sigil, single
/// line-number gutter. Spans are syntect-highlighted up front so
/// the renderer doesn't redo the syntax pass per frame.
#[derive(Debug, Clone)]
pub struct ReadLine {
    pub kind: ReadLineKind,
    pub lineno: u32,
    pub text: String,
    pub spans: Vec<HighlightedSpan>,
}

/// Background-tint classification for a read-mode row. `Plain` is
/// unchanged context; `Added`/`Modified` map to real lines in the
/// new file; `Separator` is a synthetic row inserted between two
/// surviving lines to mark "N lines were deleted here". The
/// classification comes from walking the diff cache, so it
/// requires the diff to be loaded first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadLineKind {
    #[default]
    Plain,
    Added,
    Modified,
    Separator {
        removed: usize,
    },
}
