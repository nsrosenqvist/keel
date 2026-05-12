//! Key + mouse dispatch for the command palette overlay.

use crate::tui::app::{App, ClickTarget, LaunchRejection};
use crate::tui::terminal::{ClickKind, hit_test, launch_message, resolve_click};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub async fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // Ctrl-c always quits.
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }

    match code {
        KeyCode::Esc => app.close_palette(),
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(p) = app.palette_mut() {
                p.select_prev();
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(p) = app.palette_mut() {
                p.select_next();
            }
        }
        KeyCode::Backspace => {
            if let Some(p) = app.palette_mut() {
                p.pop_char();
            }
        }
        KeyCode::Enter => {
            activate_selection(app);
        }
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(p) = app.palette_mut() {
                p.push_char(c);
            }
        }
        _ => {}
    }
}

/// Click → select; double-click → run the selected entry (same
/// path the Enter key takes in [`handle_key`]).
pub async fn handle_mouse(app: &mut App, me: MouseEvent) {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Snapshot the index under the cursor before mutating
            // the palette — drop the immutable borrow before any
            // mutation happens.
            let Some(idx) = app
                .palette()
                .and_then(|p| hit_test(&p.row_rects.borrow(), me.column, me.row))
            else {
                return;
            };
            let target = ClickTarget::PaletteRow(idx);
            match resolve_click(app, target) {
                ClickKind::Select => {
                    if let Some(p) = app.palette_mut() {
                        p.select_at(idx);
                    }
                }
                ClickKind::Activate => {
                    if let Some(p) = app.palette_mut() {
                        p.select_at(idx);
                    }
                    activate_selection(app);
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(p) = app.palette_mut() {
                p.select_next();
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(p) = app.palette_mut() {
                p.select_prev();
            }
        }
        _ => {}
    }
}

/// Run the palette's selected entry. Mirrors the `KeyCode::Enter`
/// arm of [`handle_key`]; called from both the Enter path and the
/// double-click activation.
fn activate_selection(app: &mut App) {
    match app.confirm_palette() {
        Some(Ok(())) => {}
        Some(Err(LaunchRejection::AlreadyRunning)) => {
            app.open_kill_restart_confirm();
        }
        Some(Err(rej)) => {
            app.flash(launch_message(rej));
        }
        None => {}
    }
}
