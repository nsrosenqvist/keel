//! Key + mouse dispatch for the Terminals view.

use crate::tui::app::{App, ClickTarget};
use crate::tui::terminal::{ClickKind, WHEEL_LINES, hit_test, resolve_click};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Mouse handler for the Terminals view. Click → select; double-
/// click → `terminals_confirm` (the same path Enter takes — attach
/// to the service/window or open the new-shell sentinel).
pub fn handle_mouse(app: &mut App, me: MouseEvent) {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let hit = {
                let rects = app.terminals().row_rects.borrow();
                hit_test(&rects, me.column, me.row)
            };
            let Some(idx) = hit else {
                return;
            };
            let target = ClickTarget::TerminalsRow(idx);
            match resolve_click(app, target) {
                ClickKind::Select => app.terminals_select_at(idx),
                ClickKind::Activate => {
                    app.terminals_select_at(idx);
                    app.terminals_confirm();
                }
            }
        }
        MouseEventKind::ScrollDown => {
            for _ in 0..WHEEL_LINES {
                app.terminals_select_next();
            }
        }
        MouseEventKind::ScrollUp => {
            for _ in 0..WHEEL_LINES {
                app.terminals_mut().select_prev();
            }
        }
        _ => {}
    }
}

pub async fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    match code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => app.terminals_select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.terminals_mut().select_prev(),
        KeyCode::Char('d') => app.terminals_kill_selected(),
        // `n` is the shortcut for the `+ new shell` sentinel —
        // hand-shortcut for power users who don't want to scroll
        // to the bottom of the list every time.
        KeyCode::Char('n') => app.queue_new_shell(),
        KeyCode::Enter => app.terminals_confirm(),
        _ => {}
    }
}
