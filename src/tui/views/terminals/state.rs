//! State for the Terminals view (`T`).
//!
//! Tmux-backed; one session per worktree. Windows are tmux's source
//! of truth — we don't track names ourselves; tmux's automatic-rename
//! keeps `window_name` in sync with the running foreground program
//! (`zsh`, `vim`, …).

use crate::config::Config;
use ratatui::layout::Rect;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// One tmux window as reported by `list-windows`. `name` is what
/// tmux's `#{window_name}` resolves to right now — for an
/// auto-renamed window this tracks the running command live.
/// `cwd` carries the active pane's `pane_current_path` when
/// available (`tmux list-windows -F`'s response can omit it for
/// just-spawned windows that haven't launched a process yet).
/// `has_bell` mirrors tmux's `#{window_bell_flag}` — set when a
/// program in the window emitted BEL (coding agents do this to
/// grab attention) and auto-cleared by tmux when the window
/// becomes current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub cwd: Option<String>,
    pub has_bell: bool,
    /// Devcontainer workspace folder, read from the `@ampelos_workspace`
    /// tmux window option ampelos set when it created the window.
    /// Empty for windows ampelos didn't tag (host shells, service
    /// attaches, pre-existing windows). Surfaces in the sidebar in
    /// place of the host-side `cwd` since the latter is just the
    /// docker client's pwd, not anything useful to the user.
    pub workspace: Option<String>,
}

/// One visible row in the Terminals view's sidebar. Services first
/// (mapped to compose-exec windows when active), then user shell
/// windows, then the `+ new shell` sentinel.
#[derive(Debug, Clone)]
pub enum TerminalsRow {
    Service(String),
    Window(TmuxWindow),
    NewSentinel,
}

/// Terminals-view state. Owns the tmux session name + cached window
/// list + selection + bell-edge tracking. Cross-context concerns
/// (which services to interleave, queueing attach requests, etc.)
/// live on [`crate::tui::app::App`] and call into this view through
/// the accessors it exposes there.
#[derive(Debug, Clone)]
pub struct TerminalsView {
    /// `None` until probed; `Some(false)` when tmux is missing.
    pub tmux_available: Option<bool>,
    /// `ampelos-<project>-<slug>` — stable per worktree.
    pub session_name: String,
    /// Live window list from `tmux list-windows`. Refreshed on view
    /// entry, after each attach return, and after a delete. Empty
    /// until the first refresh — and stays empty when the tmux
    /// session doesn't exist yet (sentinel + service rows still
    /// render fine).
    pub windows: Vec<TmuxWindow>,
    /// Last captured tmux pane content per window index, refreshed
    /// alongside the windows list. Renders in the right pane as
    /// a preview of what's running, so the user sees more than
    /// "press enter to attach" once a window has been used.
    pub previews: HashMap<u32, Vec<String>>,
    /// Selected index across the (services + windows + sentinel)
    /// concatenation that the renderer / keymap iterate.
    pub selected: usize,
    /// Last-observed `has_bell` per window. Drives edge-triggered
    /// bell forwarding: when a window goes false→true we emit BEL
    /// to the outer terminal so the user's terminal emulator can
    /// trigger its own notification action (audible bell, OS
    /// notification, dock badge, etc.). Pruned to the live window
    /// set on every update.
    pub previous_bell: HashMap<u32, bool>,
    /// True when [`TerminalsView::set_windows`] just observed at
    /// least one false→true bell transition. The render loop consumes
    /// this after the next draw via
    /// [`TerminalsView::take_pending_bell`] and writes `\x07` to
    /// stdout — once per event, no matter how many windows flipped at
    /// the same time.
    pub pending_bell_emit: bool,
    /// Set when the next [`TerminalsView::set_windows`] call should
    /// resync `previous_bell` without emitting. Used right after
    /// attach return: bells that fired during the attach already
    /// played through tmux's `bell-action any`, so we don't want
    /// ampelos to beep again on its way back to the TUI.
    pub suppress_next_bell_emit: bool,
    /// Per-row rects for the sidebar (services + windows + sentinel),
    /// in the same global-index order the keymap uses. Populated by
    /// the renderer each frame; hit-tested by the mouse handler to
    /// route clicks to a row index.
    pub row_rects: RefCell<Vec<Rect>>,
}

impl TerminalsView {
    /// Build initial state for a project. The window list starts
    /// empty — the view populates it on first entry by querying
    /// tmux. Session name is derived from the project name; slug-
    /// aware naming would require the runtime identity, which the
    /// caller can layer in via `App::with_project_root`.
    pub fn default_for(config: &Config) -> Self {
        let project = config.project.name.as_deref().unwrap_or("ampelos");
        Self {
            tmux_available: None,
            session_name: format!("ampelos-{project}"),
            windows: Vec::new(),
            previews: HashMap::new(),
            selected: 0,
            previous_bell: HashMap::new(),
            pending_bell_emit: false,
            suppress_next_bell_emit: false,
            row_rects: RefCell::new(Vec::new()),
        }
    }

    pub fn set_tmux_available(&mut self, available: bool) {
        self.tmux_available = Some(available);
    }

    pub fn set_preview(&mut self, index: u32, lines: Vec<String>) {
        self.previews.insert(index, lines);
    }

    pub fn preview(&self, index: u32) -> Option<&Vec<String>> {
        self.previews.get(&index)
    }

    /// True if a fresh bell transition has been observed and not yet
    /// drained by the render loop. Clears on read. The event loop
    /// calls this after each draw and writes `\x07` to stdout when it
    /// returns true.
    pub fn take_pending_bell(&mut self) -> bool {
        std::mem::replace(&mut self.pending_bell_emit, false)
    }

    /// Arm the silence flag so the next [`Self::set_windows`] call
    /// resyncs the bell baseline without emitting. Used by the
    /// attach-return path — those bells already rang through tmux's
    /// `bell-action any`.
    pub fn silence_next_bell(&mut self) {
        self.suppress_next_bell_emit = true;
    }

    pub fn select_next(&mut self, total_rows: usize) {
        if total_rows > 0 {
            self.selected = (self.selected + 1).min(total_rows - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move selection to `idx`, clamped to the last row. No-op when
    /// the row list is empty (it never is during normal operation —
    /// the `+ new shell` sentinel always renders — but guarding here
    /// keeps the API uniform with the other `select_at` variants).
    pub fn select_at(&mut self, idx: usize, total_rows: usize) {
        if total_rows == 0 {
            return;
        }
        self.selected = idx.min(total_rows - 1);
    }

    /// Replace the cached tmux window list. Caller has just queried
    /// `tmux list-windows`; we adopt the result, prune previews and
    /// the bell baseline for windows that no longer exist, and clamp
    /// selection so it can't dangle past the list's end after a
    /// delete.
    ///
    /// Side effect: detects per-window `has_bell` transitions from
    /// false→true against `previous_bell` and arms `pending_bell_emit`
    /// when at least one fresh bell lands, so the render loop emits
    /// `\x07` once on the next pass. If `suppress_next_bell_emit` is
    /// set (e.g. right after attach return), we resync the baseline
    /// without arming — those bells already played through tmux's
    /// `bell-action any` during the attach.
    ///
    /// `total_rows_after` is the row count after the swap (services +
    /// new windows + sentinel) so the clamp can land exactly on the
    /// last row of the new list. The caller owns the math because
    /// services live on [`crate::tui::app::App`].
    pub fn set_windows(&mut self, windows: Vec<TmuxWindow>, total_rows_after: usize) {
        let live: HashSet<u32> = windows.iter().map(|w| w.index).collect();
        let mut fresh_bell = false;
        for w in &windows {
            let prev = self.previous_bell.get(&w.index).copied().unwrap_or(false);
            if w.has_bell && !prev {
                fresh_bell = true;
            }
            self.previous_bell.insert(w.index, w.has_bell);
        }
        self.previous_bell.retain(|k, _| live.contains(k));
        if self.suppress_next_bell_emit {
            self.suppress_next_bell_emit = false;
        } else if fresh_bell {
            self.pending_bell_emit = true;
        }
        self.previews.retain(|k, _| live.contains(k));
        self.windows = windows;
        if total_rows_after > 0 && self.selected >= total_rows_after {
            self.selected = total_rows_after - 1;
        }
    }
}
