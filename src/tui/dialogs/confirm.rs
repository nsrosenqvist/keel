//! Yes/No confirmation dialog state.

use crate::tui::app::RunKey;

/// Pending decision when the user tries to launch a running command.
/// Kept simple: only one in-flight question at a time, only one kind
/// of question (kill-and-restart). New question shapes get added here
/// when they arrive.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub body: String,
    /// Currently-focused choice. `true` = Yes (the default).
    pub yes_focused: bool,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Abort the named run and relaunch it.
    KillAndRestart { key: RunKey },
    /// Kill a tmux window in the Terminals view. Carries the
    /// session + index so the action survives any navigation the
    /// user does while the modal is open.
    KillTmuxWindow {
        session: String,
        index: u32,
        /// Window name as it was when the modal opened — purely
        /// for the dialog body so the user sees what they're
        /// killing.
        name: String,
    },
}
