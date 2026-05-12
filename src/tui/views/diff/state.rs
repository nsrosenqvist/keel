//! State for the diff view (`g`).

use crate::tui::shared::scroll::{Axis, BodyScroll};
use crate::tui::syntax::HighlightedSpan;
use crate::tui::views::diff::line_width::{diff_line_rendered_width, read_line_rendered_width};
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

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

/// State for the Diff view (`g`). Populated lazily on first
/// switch, rebuilt on `r`. Per-file diff bodies cache so
/// navigating among files doesn't re-shell-out to git.
#[derive(Debug, Clone, Default)]
pub struct DiffView {
    pub files: Vec<DiffFile>,
    pub selected: usize,
    pub cache: HashMap<String, Vec<DiffLine>>,
    /// True once `files` has been populated at least once. Lets the
    /// renderer distinguish "no changes" from "haven't checked yet".
    pub loaded: bool,
    /// Last error from `git status` / `git diff`, if any.
    pub error: Option<String>,
    /// Trunk branch the diff is scoped against (e.g. "main"). None
    /// when no trunk could be detected — diff falls back to
    /// `git diff HEAD` and the top bar omits the `vs <trunk>` slot.
    pub trunk: Option<String>,
    /// SHA of the merge-base between `trunk` and HEAD. The diff
    /// commands compare working tree against this anchor so users
    /// see "everything I've changed since branching off trunk"
    /// rather than just the working-tree-vs-last-commit slice.
    /// None → fall back to HEAD as the anchor.
    pub anchor: Option<String>,
    /// 7-char short form of `anchor`, surfaced in the header banner
    /// so users can spot when a freshly-pulled trunk shifts the
    /// comparison forward without hunting for the SHA themselves.
    pub anchor_short: Option<String>,
    /// The current branch name (`git rev-parse --abbrev-ref HEAD`).
    /// Different from `App::branch`: that one is set by the CLI at
    /// startup (and may carry a detached-HEAD basename); this one
    /// is refreshed on every diff reload so amends / checkouts
    /// inside the same TUI session are reflected.
    pub branch: Option<String>,
    /// Sum of `additions` across `files` — the headline `+N` in
    /// the comparison banner.
    pub additions_total: usize,
    pub deletions_total: usize,
    /// Which pane has focus. Tab toggles.
    pub focus: DiffFocus,
    /// Scroll offsets (vertical + horizontal) for the diff body mode,
    /// keyed by file path. Switching modes preserves each mode's pan
    /// + scroll position.
    pub diff_scroll: BodyScroll,
    /// Scroll offsets for the read body mode.
    pub read_scroll: BodyScroll,
    /// Last viewport height the body was rendered at. Cell so the
    /// renderer can write through `&DiffView`. Read by PgUp/PgDn and
    /// G to size half-pages and clamp to the bottom of the diff.
    pub body_height: Cell<u16>,
    /// Last viewport width the body was rendered at. Used by the
    /// horizontal-scroll clamp so panning stops when the rightmost
    /// column of the longest row reaches the right edge of the
    /// viewport. Stays at 0 until the first body render — h-scroll
    /// silently clamps to 0 in that case (no body yet to scroll).
    pub body_width: Cell<u16>,
    /// Last frame's outer rect for the files-list pane and the
    /// diff-body pane. None when the diff view isn't being rendered,
    /// so a stale rect from a previous view can't match a wheel
    /// event against the current view's geometry. Mouse routing
    /// hit-tests against these.
    pub files_rect: Cell<Option<Rect>>,
    pub body_rect: Cell<Option<Rect>>,
    /// Wrap long diff lines (`w`). Off by default — most diffs are
    /// readable without wrapping and horizontal "loss" is preferred
    /// over visual jitter. On for narrow terminals.
    pub wrap: bool,
    /// Per-file row rects in the files list. Populated by the
    /// renderer on every frame; hit-tested by the mouse handler to
    /// route clicks to a file index.
    pub file_row_rects: RefCell<Vec<Rect>>,
    /// True when `lazygit` was found on PATH at startup; the `L`
    /// keybind hides itself from the footer hint when false.
    pub lazygit_available: bool,
    /// Two-key chord state: timestamp of the last `g` press while
    /// body-focused. A second `g` within 500 ms triggers gg → top.
    /// Cleared by any other key.
    pub last_g_press: Option<std::time::Instant>,
    /// Whether the body pane is currently showing the diff or the
    /// full file. Toggled with `v`. Global, not per-file — toggling
    /// flips the active file's view in place.
    pub body_mode: BodyMode,
    /// Per-file cache of full-file contents for read mode. Lazily
    /// populated on the first switch into read mode for a given
    /// file; cleared by `r` (refresh) alongside the diff cache.
    pub read_cache: HashMap<String, Vec<ReadLine>>,
}

impl DiffView {
    /// Replace the file list (typically after a `git status` reload).
    /// Clamps the selection so it can't point past the end. Cache
    /// stays — the user might re-edit and want the same diff back.
    /// Recomputes the additions / deletions totals from the
    /// per-file numbers so the comparison banner stays in sync.
    pub fn set_files(&mut self, files: Vec<DiffFile>) {
        self.additions_total = files.iter().map(|f| f.additions).sum();
        self.deletions_total = files.iter().map(|f| f.deletions).sum();
        self.files = files;
        self.loaded = true;
        if self.selected >= self.files.len() {
            self.selected = self.files.len().saturating_sub(1);
        }
        self.error = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        self.loaded = true;
    }

    /// Pin the trunk branch + merge-base SHA the diff loaders should
    /// use as their anchor. Either may be None: a trunk without a
    /// merge-base means the trunk exists but has no shared history
    /// (rare); no trunk at all means we degrade to `git diff HEAD`.
    /// `branch` is the current HEAD branch and `anchor_short` is the
    /// 7-char form of the SHA — both surface in the comparison
    /// banner.
    pub fn set_anchor(
        &mut self,
        trunk: Option<String>,
        anchor: Option<String>,
        branch: Option<String>,
        anchor_short: Option<String>,
    ) {
        self.trunk = trunk;
        self.anchor = anchor;
        self.branch = branch;
        self.anchor_short = anchor_short;
    }

    /// Set whether `lazygit` is on PATH. Called once at startup.
    pub fn set_lazygit_available(&mut self, available: bool) {
        self.lazygit_available = available;
    }

    pub fn select_next(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.files.len() - 1);
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Set the file selection to `idx`, clamped to the last file.
    /// No-op when the file list is empty.
    pub fn select_at(&mut self, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        self.selected = idx.min(self.files.len() - 1);
    }

    pub fn selected_file(&self) -> Option<&DiffFile> {
        self.files.get(self.selected)
    }

    pub fn cache_for(&self, path: &str) -> Option<&Vec<DiffLine>> {
        self.cache.get(path)
    }

    pub fn set_cache(&mut self, path: String, lines: Vec<DiffLine>) {
        self.cache.insert(path, lines);
    }

    pub fn read_cache_for(&self, path: &str) -> Option<&Vec<ReadLine>> {
        self.read_cache.get(path)
    }

    pub fn set_read_cache(&mut self, path: String, lines: Vec<ReadLine>) {
        self.read_cache.insert(path, lines);
    }

    pub fn body_mode(&self) -> BodyMode {
        self.body_mode
    }

    /// Flip the body pane between Diff and Read. Drops any pending
    /// `g` chord so a half-armed gg doesn't carry across modes.
    pub fn toggle_body_mode(&mut self) {
        self.body_mode = match self.body_mode {
            BodyMode::Diff => BodyMode::Read,
            BodyMode::Read => BodyMode::Diff,
        };
        self.last_g_press = None;
    }

    /// Mark the diff state stale so the next render pulls fresh
    /// data. Used by the `r` keybind in the diff view. Clears both
    /// caches so a toggled view also re-fetches after refresh.
    pub fn mark_stale(&mut self) {
        self.loaded = false;
        self.cache.clear();
        self.diff_scroll.clear();
        self.read_cache.clear();
        self.read_scroll.clear();
        self.additions_total = 0;
        self.deletions_total = 0;
    }

    pub fn focus(&self) -> DiffFocus {
        self.focus
    }

    /// Toggle keyboard focus between the file list and the diff
    /// body. Wired to Tab / Shift+Tab.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            DiffFocus::Files => DiffFocus::Body,
            DiffFocus::Body => DiffFocus::Files,
        };
        // Drop any pending `g` chord on focus change so a stale `g`
        // can't hop to top after the user already moved on.
        self.last_g_press = None;
    }

    /// Set focus directly. Used by handlers that have semantic
    /// intent (e.g. Enter on the file list moves into the body).
    pub fn set_focus(&mut self, focus: DiffFocus) {
        self.focus = focus;
        self.last_g_press = None;
    }

    /// Current scroll offset (top line) for the selected file in
    /// the active body mode.
    pub fn body_scroll(&self) -> usize {
        let path = match self.selected_file() {
            Some(f) => &f.path,
            None => return 0,
        };
        self.active_scroll().get(path, Axis::Vertical)
    }

    /// Move the body scroll by `delta` lines, clamped to the
    /// content length (per active mode) minus the last viewport
    /// height. Negative values scroll up.
    pub fn body_scroll_by(&mut self, delta: i32) {
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let total = self.active_content_len(&path);
        let viewport = self.body_height.get() as usize;
        let max = total.saturating_sub(viewport.max(1));
        self.active_scroll_mut()
            .scroll_by(&path, Axis::Vertical, delta, max);
    }

    pub fn body_scroll_to_top(&mut self) {
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        self.active_scroll_mut().set(&path, Axis::Vertical, 0);
    }

    pub fn body_scroll_to_bottom(&mut self) {
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let total = self.active_content_len(&path);
        let viewport = self.body_height.get() as usize;
        let bottom = total.saturating_sub(viewport.max(1));
        self.active_scroll_mut().set(&path, Axis::Vertical, bottom);
    }

    fn active_scroll(&self) -> &BodyScroll {
        match self.body_mode {
            BodyMode::Diff => &self.diff_scroll,
            BodyMode::Read => &self.read_scroll,
        }
    }

    fn active_scroll_mut(&mut self) -> &mut BodyScroll {
        match self.body_mode {
            BodyMode::Diff => &mut self.diff_scroll,
            BodyMode::Read => &mut self.read_scroll,
        }
    }

    /// Max rendered width (in columns) across every line of the
    /// active body mode's cache for `path`. Returns 0 when the cache
    /// is missing — h-scroll then clamps to 0, which is the safest
    /// pre-load behavior.
    fn active_max_line_width(&self, path: &str) -> usize {
        match self.body_mode {
            BodyMode::Diff => {
                let Some(lines) = self.cache.get(path) else {
                    return 0;
                };
                let max_lineno = lines
                    .iter()
                    .filter_map(|l| l.new_lineno.map(u64::from).or(l.old_lineno.map(u64::from)))
                    .max()
                    .unwrap_or(0);
                let gutter_w = max_lineno.to_string().len().max(1);
                lines
                    .iter()
                    .map(|l| diff_line_rendered_width(l, gutter_w))
                    .max()
                    .unwrap_or(0)
            }
            BodyMode::Read => {
                let Some(lines) = self.read_cache.get(path) else {
                    return 0;
                };
                let max_lineno = lines.iter().map(|l| l.lineno).max().unwrap_or(0);
                let gutter_w = max_lineno.to_string().len().max(1);
                lines
                    .iter()
                    .map(|l| read_line_rendered_width(l, gutter_w))
                    .max()
                    .unwrap_or(0)
            }
        }
    }

    /// Upper bound on `body_h_scroll` for the selected file: the
    /// position where the rightmost char of the longest row sits at
    /// the right edge of the viewport. 0 when the longest line
    /// fits within the viewport (no panning needed). 0 also
    /// pre-render (body_width starts at 0) — h-scroll stays parked
    /// at column 0 until the first frame lands.
    fn max_h_scroll(&self, path: &str) -> usize {
        let max_line = self.active_max_line_width(path);
        let viewport = self.body_width.get() as usize;
        max_line.saturating_sub(viewport)
    }

    /// Current horizontal scroll offset (columns from x=0) for the
    /// selected file in the active body mode. Always returns `0`
    /// when wrap is on — wrap mode has no horizontal axis, and a
    /// stale map entry must not bleed into rendering. Also clamps
    /// against the longest-line upper bound so a terminal resize
    /// that shrinks the max can't leave the renderer with a stale
    /// out-of-range value.
    pub fn body_h_scroll(&self) -> usize {
        if self.wrap {
            return 0;
        }
        let path = match self.selected_file() {
            Some(f) => &f.path,
            None => return 0,
        };
        self.active_scroll()
            .get(path, Axis::Horizontal)
            .min(self.max_h_scroll(path))
    }

    /// Pan the body by `delta` columns. Clamped at 0 (no left of
    /// column zero) and at the rightmost column of the longest row
    /// (no panning into empty space). No-op while wrap is on, so
    /// wheel events during wrap mode can't dirty the map.
    pub fn body_h_scroll_by(&mut self, delta: i32) {
        if self.wrap {
            return;
        }
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let max = self.max_h_scroll(&path);
        self.active_scroll_mut()
            .scroll_by(&path, Axis::Horizontal, delta, max);
    }

    fn active_content_len(&self, path: &str) -> usize {
        match self.body_mode {
            BodyMode::Diff => self.cache.get(path).map(|v| v.len()).unwrap_or(0),
            BodyMode::Read => self.read_cache.get(path).map(|v| v.len()).unwrap_or(0),
        }
    }

    /// Jump to the next `@@` hunk header below the current scroll
    /// position. No-op if no further hunk exists. Hunk navigation is
    /// only meaningful in diff mode (read mode has no hunk markers),
    /// so this always operates on `diff_scroll`.
    pub fn jump_hunk_next(&mut self) {
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let Some(lines) = self.cache.get(&path) else {
            return;
        };
        let cur = self.diff_scroll.get(&path, Axis::Vertical);
        let next = lines
            .iter()
            .enumerate()
            .skip(cur + 1)
            .find(|(_, l)| l.kind == DiffLineKind::Hunk)
            .map(|(i, _)| i);
        if let Some(i) = next {
            let viewport = self.body_height.get() as usize;
            let max = lines.len().saturating_sub(viewport.max(1));
            self.diff_scroll.set(&path, Axis::Vertical, i.min(max));
        }
    }

    /// Jump to the previous `@@` hunk header above the current
    /// scroll position. No-op at the top.
    pub fn jump_hunk_prev(&mut self) {
        let Some(file) = self.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let Some(lines) = self.cache.get(&path) else {
            return;
        };
        let cur = self.diff_scroll.get(&path, Axis::Vertical);
        let prev = lines
            .iter()
            .enumerate()
            .take(cur)
            .rev()
            .find(|(_, l)| l.kind == DiffLineKind::Hunk)
            .map(|(i, _)| i);
        if let Some(i) = prev {
            self.diff_scroll.set(&path, Axis::Vertical, i);
        }
    }

    pub fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        // Turning wrap *on* with a non-zero h-offset would silently
        // chop the first N columns of every wrapped row. Clear the
        // active file's entry in both modes to keep the model honest;
        // `body_h_scroll` also clamps to 0 while wrap is on as a
        // belt-and-suspenders against any other code path that
        // mutates the offset without going through this toggle.
        if self.wrap {
            let Some(file) = self.files.get(self.selected) else {
                return;
            };
            let path = file.path.clone();
            self.diff_scroll.remove(&path, Axis::Horizontal);
            self.read_scroll.remove(&path, Axis::Horizontal);
        }
    }

    /// Record a `g` press while body-focused. A second `g` within
    /// 500 ms returns true (caller jumps to top); otherwise false
    /// (caller arms the chord by storing the timestamp).
    pub fn consume_g_chord(&mut self) -> bool {
        let now = std::time::Instant::now();
        let window = std::time::Duration::from_millis(500);
        let armed = self
            .last_g_press
            .map(|t| now.duration_since(t) <= window)
            .unwrap_or(false);
        self.last_g_press = if armed { None } else { Some(now) };
        armed
    }
}
