//! Key + mouse dispatch for the yes/no confirmation modal.

use crate::tui::app::App;
use crate::tui::terminal::{launch_message, rect_contains};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // Ctrl-c always quits even from a modal.
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    match code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm_resolve(false);
        }
        KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
            app.confirm_toggle_focus();
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(rej) = app.confirm_resolve(true) {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Enter => {
            // Enter accepts the focused choice — Yes by default;
            // if the user tabbed to No, Enter dismisses.
            let accept = app.confirm_dialog().map(|d| d.yes_focused).unwrap_or(true);
            if let Some(rej) = app.confirm_resolve(accept) {
                app.flash(launch_message(rej));
            }
        }
        _ => {}
    }
}

/// Single-click on Yes / No presses the corresponding button;
/// nothing else (no concept of "selected" button — the keyboard
/// moves focus, the mouse acts directly).
pub fn handle_mouse(app: &mut App, me: MouseEvent) {
    let MouseEventKind::Down(MouseButton::Left) = me.kind else {
        return;
    };
    let yes_hit = app
        .confirm_yes_rect
        .get()
        .is_some_and(|r| rect_contains(r, me.column, me.row));
    let no_hit = app
        .confirm_no_rect
        .get()
        .is_some_and(|r| rect_contains(r, me.column, me.row));
    if yes_hit {
        if let Some(rej) = app.confirm_resolve(true) {
            app.flash(launch_message(rej));
        }
    } else if no_hit {
        app.confirm_resolve(false);
    }
}
