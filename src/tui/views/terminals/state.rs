//! Leaf state types for the terminals view.
//!
//! `TmuxWindow` mirrors one tmux window as reported by
//! `list-windows`. `TerminalsRow` is what the sidebar iterates —
//! services first, then user shell windows, then the `+ new shell`
//! sentinel.

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
    /// Devcontainer workspace folder, read from the `@keel_workspace`
    /// tmux window option keel set when it created the window.
    /// Empty for windows keel didn't tag (host shells, service
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
