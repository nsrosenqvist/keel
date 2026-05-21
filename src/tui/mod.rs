//! croft TUI — embedded dashboard.
//!
//! First slice (this commit): a navigable browser. Sidebar lists every
//! recipe and script declared in the project; the right-hand detail pane
//! shows what each one would do. No execution from the TUI yet — that
//! arrives in the next slice once the executor learns to stream output
//! into a buffer instead of inheriting stdout.
//!
//! Architecture:
//!
//! - [`app::App`] holds state (item list, selection, scroll, quit flag).
//! - [`ui::render`] is a pure function from `App -> Frame`. No state mutation
//!   in the renderer, no I/O in the model.
//! - [`run`] owns the terminal lifecycle (raw mode, alternate screen) and
//!   drives the event loop. Crossterm events arrive via a blocking polling
//!   thread piped through an mpsc channel.

use crate::config::Config;
use crate::container::Backend;
use crate::runtime::Executor;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

pub mod ansi;
pub mod app;
pub mod dialogs;
pub mod palette;
pub mod runner;
pub mod services;
pub mod shared;
pub mod syntax;
pub mod ui;
pub mod views;
pub mod watchers;
pub mod worker;

pub mod editor;
mod lazygit;
mod terminal;
#[cfg(test)]
mod test_support;

pub use app::{App, View};
pub use runner::{CapturedLine, RunState};
pub use services::ServicePane;
pub use views::control_center::state::{Item, ItemKind};
pub use watchers::{WatcherPane, WatcherState};

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal io: {0}")]
    Io(#[from] std::io::Error),
}

/// Outcome from one TUI session. The CLI's outer loop rebuilds
/// against the new project root when the user picks a worktree from
/// the switcher; otherwise the session ends.
pub use terminal::DriveOutcome;

/// Open the TUI dashboard. Returns when the user quits or signals a
/// worktree switch. The caller's job to rebuild the App against the
/// new root and call `run` again — that keeps the TUI crate
/// independent of how config / backend / executor are constructed.
///
/// `initial_view` lets the CLI carry the active view across a
/// worktree hot-reload so the user lands where they left off
/// rather than always returning to the control center.
///
/// `branch` populates the top-bar branch slot (None → header skips
/// the slot, e.g. when not in a git repo). The diff file list, the
/// auto-discovered service list, and the watcher panes all start
/// loading on background tasks — the first frame paints before any
/// of them finish, then [`App::drain_boot_results`] folds the
/// results in as they arrive.
pub async fn run(
    config: Arc<Config>,
    executor: Executor,
    backend: Arc<dyn Backend>,
    devcontainer: Option<Arc<crate::container::devcontainer::DevcontainerBackend>>,
    project_root: &Path,
    initial_view: View,
    branch: Option<String>,
) -> Result<DriveOutcome, TuiError> {
    let editor_cfg = editor::resolve(&config.editor);
    let mut app = App::new(config)
        .with_executor(executor)
        .with_backend(backend)
        .with_project_root(project_root)
        .with_branch(branch)
        .with_editor(editor_cfg);
    if let Some(dc) = devcontainer {
        app = app.with_devcontainer(dc);
    }
    app.spawn_boot_tasks(project_root);
    if initial_view != View::ControlCenter {
        app.switch_view(initial_view);
    }
    terminal::run_event_loop(&mut app, initial_view).await
}
