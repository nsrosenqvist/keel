//! Shared test helpers for handler-level tests.
//!
//! Phases 6 and 10 reshape dispatch in ways that the existing
//! parsing-helper tests can't catch. This module gives every
//! view-level test suite a uniform way to synthesise events and
//! drive them through the real `handle_event` path so handler
//! regressions surface as failing tests rather than runtime bugs
//! noticed by a user.
//!
//! Three primitives:
//!   - [`synth_press`] / [`synth_mouse`] build `Event` values
//!     equivalent to what crossterm would deliver from a real
//!     terminal.
//!   - [`apply_event`] drives the event through `handle_event` on a
//!     current-thread tokio runtime — synchronous from the caller's
//!     POV, so tests don't need `#[tokio::test]`.
//!
//! All three are `pub(crate)` because they cross module boundaries
//! (input.rs tests in `views/*/`) but aren't part of the ampelos
//! library's external surface.

use crate::tui::app::App;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

/// Build an `Event::Key(Press)` for `code` with `modifiers`.
pub(crate) fn synth_press(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// Build an `Event::Mouse` at column / row with `kind` and
/// `modifiers`. Use [`MouseEventKind::Down`] / `Up` /
/// `ScrollUp` / `ScrollDown` / etc.
#[allow(dead_code)]
pub(crate) fn synth_mouse(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

/// Convenience: synthesise a left-click at (col, row) with no
/// modifiers.
#[allow(dead_code)]
pub(crate) fn synth_click(column: u16, row: u16) -> Event {
    synth_mouse(
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        KeyModifiers::NONE,
    )
}

/// Drive `event` through the real `handle_event` path on a
/// current-thread tokio runtime. Synchronous from the caller's
/// POV — tests don't need `#[tokio::test]`.
pub(crate) fn apply_event(app: &mut App, event: Event) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    rt.block_on(crate::tui::terminal::handle_event(app, event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, Mode, View};
    use std::sync::Arc;

    fn cfg() -> Arc<Config> {
        Arc::new(
            crate::config::parse_str(
                r#"
                [runtime]
                backend = "none"

                [command.greet]
                run = "echo hi"

                [command.build]
                run = "cargo build"
                "#,
            )
            .unwrap(),
        )
    }

    /// `T` from the control center lands in the terminals view.
    /// Phase 6's command-queue rewrite mustn't break view switching;
    /// Phase 10's actor-model conversion mustn't either.
    #[test]
    fn capital_t_switches_to_terminals() {
        let mut app = App::new(cfg());
        assert_eq!(app.view(), View::ControlCenter);
        apply_event(
            &mut app,
            synth_press(KeyCode::Char('T'), KeyModifiers::NONE),
        );
        assert_eq!(app.view(), View::Terminals);
    }

    /// `G` from the control center lands in the diff view.
    #[test]
    fn capital_g_switches_to_diff() {
        let mut app = App::new(cfg());
        apply_event(
            &mut app,
            synth_press(KeyCode::Char('G'), KeyModifiers::NONE),
        );
        assert_eq!(app.view(), View::Diff);
    }

    /// `/` from the control center opens the command palette. The
    /// Phase 4 modal-fusion mustn't have desynced this; Phase 6's
    /// flash encapsulation shouldn't either.
    #[test]
    fn slash_opens_palette() {
        let mut app = App::new(cfg());
        apply_event(
            &mut app,
            synth_press(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert_eq!(app.mode(), Mode::Palette);
        assert!(app.palette().is_some());
    }

    /// `Esc` closes the open palette and routes keys back to the
    /// active view's handler. Asserts both `mode()` flips back to
    /// `Normal` and the `palette()` accessor returns `None` — the
    /// pair the Phase 4 fusion is supposed to keep impossible to
    /// desync.
    #[test]
    fn esc_closes_palette() {
        let mut app = App::new(cfg());
        apply_event(
            &mut app,
            synth_press(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert_eq!(app.mode(), Mode::Palette);
        apply_event(&mut app, synth_press(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.palette().is_none());
    }
}
