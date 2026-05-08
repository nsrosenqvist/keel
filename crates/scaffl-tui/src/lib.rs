//! scaffl TUI — embedded dashboard.
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

use scaffl_config::Config;
use scaffl_runtime::Executor;
use std::sync::Arc;
use thiserror::Error;

pub mod app;
pub mod runner;
pub mod ui;

mod terminal;

pub use app::{App, Item, ItemKind};
pub use runner::{CapturedLine, RunState};

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal io: {0}")]
    Io(#[from] std::io::Error),
}

/// Open the TUI dashboard. Returns when the user quits.
pub async fn run(config: Arc<Config>, executor: Executor) -> Result<(), TuiError> {
    let mut app = App::new(config).with_executor(executor);
    terminal::run_event_loop(&mut app).await
}
