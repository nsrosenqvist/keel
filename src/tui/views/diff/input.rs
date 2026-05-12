//! Key + mouse dispatch for the Diff view.

use crate::tui::app::{App, ClickTarget};
use crate::tui::terminal::{WHEEL_LINES, hit_test, rect_contains, resolve_click};
use crate::tui::views::diff::git::{
    ensure_diff_for_selected, ensure_diff_loaded, ensure_read_for_selected,
};
use crate::tui::views::diff::state::{BodyMode, DiffFocus};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Columns per horizontal-wheel notch. Larger than `WHEEL_LINES`
/// because columns are narrower than lines visually — 8 feels like a
/// trackpad swipe's natural step.
const HSCROLL_COLS: i32 = 8;

/// Click on a file row jumps to that file; ScrollUp/Down with cursor
/// on cursor pane, Shift+wheel (or native horizontal trackpad
/// swipes) pans the body horizontally.
pub async fn handle_mouse(app: &mut App, me: MouseEvent) {
    let files_rect = app.diff().files_rect.get();
    let body_rect = app.diff().body_rect.get();

    let over_body = body_rect.is_some_and(|r| rect_contains(r, me.column, me.row));
    let over_files = files_rect.is_some_and(|r| rect_contains(r, me.column, me.row));
    let shift = me.modifiers.contains(KeyModifiers::SHIFT);

    match me.kind {
        // Click on the files list: jump to the clicked file. Single-
        // click is enough — selection alone re-renders the body to
        // that file, so there's nothing extra a double-click would
        // do today.
        MouseEventKind::Down(MouseButton::Left) if over_files => {
            let hit = {
                let rects = app.diff().file_row_rects.borrow();
                hit_test(&rects, me.column, me.row)
            };
            if let Some(idx) = hit {
                app.diff_mut().select_at(idx);
                // Mirror the keyboard path: trigger the lazy load so
                // the body actually shows the clicked file's diff
                // instead of sitting on "loading diff…".
                ensure_diff_for_selected(app).await;
                // Resolve so a future double-click on the same row
                // could activate (no semantic activation today, but
                // the bookkeeping is uniform with other surfaces).
                let _ = resolve_click(app, ClickTarget::DiffFile(idx));
            }
        }
        // Horizontal pan: native left/right wheel from a trackpad,
        // or Shift+vertical-wheel as the wheel-only-mouse fallback.
        // Gated on body focus so a click on the files list still
        // scrolls files, not the body's horizontal axis.
        MouseEventKind::ScrollRight if over_body => app.diff_mut().body_h_scroll_by(HSCROLL_COLS),
        MouseEventKind::ScrollLeft if over_body => app.diff_mut().body_h_scroll_by(-HSCROLL_COLS),
        MouseEventKind::ScrollDown if over_body && shift => {
            app.diff_mut().body_h_scroll_by(HSCROLL_COLS);
        }
        MouseEventKind::ScrollUp if over_body && shift => {
            app.diff_mut().body_h_scroll_by(-HSCROLL_COLS)
        }
        MouseEventKind::ScrollDown if over_body => app.diff_mut().body_scroll_by(WHEEL_LINES),
        MouseEventKind::ScrollUp if over_body => app.diff_mut().body_scroll_by(-WHEEL_LINES),
        MouseEventKind::ScrollDown if over_files => {
            for _ in 0..WHEEL_LINES {
                app.diff_mut().select_next();
            }
            ensure_diff_for_selected(app).await;
        }
        MouseEventKind::ScrollUp if over_files => {
            for _ in 0..WHEEL_LINES {
                app.diff_mut().select_prev();
            }
            ensure_diff_for_selected(app).await;
        }
        _ => {}
    }
}

pub async fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    // Always-on bindings: shared across both focused panes.
    match code {
        KeyCode::Char('q') => {
            app.quit();
            return;
        }
        KeyCode::Char('r') => {
            app.diff_mut().mark_stale();
            ensure_diff_loaded(app).await;
            return;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.diff_mut().toggle_focus();
            return;
        }
        KeyCode::Char('w') => {
            app.diff_mut().toggle_wrap();
            return;
        }
        KeyCode::Char('v') => {
            app.diff_mut().toggle_body_mode();
            if app.diff().body_mode() == BodyMode::Read {
                ensure_read_for_selected(app).await;
            }
            return;
        }
        KeyCode::Char('L') => {
            if app.diff().lazygit_available {
                app.request_lazygit();
            } else {
                app.flash("install lazygit to enable the L keybind");
            }
            return;
        }
        KeyCode::Char(']') => {
            // Hunk jump is meaningless in read mode — drop the key
            // rather than writing into the wrong scroll map.
            if app.diff().body_mode() == BodyMode::Read {
                return;
            }
            app.diff_mut().set_focus(DiffFocus::Body);
            app.diff_mut().jump_hunk_next();
            return;
        }
        KeyCode::Char('[') => {
            if app.diff().body_mode() == BodyMode::Read {
                return;
            }
            app.diff_mut().set_focus(DiffFocus::Body);
            app.diff_mut().jump_hunk_prev();
            return;
        }
        _ => {}
    }
    match app.diff().focus() {
        DiffFocus::Files => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                app.diff_mut().select_next();
                ensure_diff_for_selected(app).await;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.diff_mut().select_prev();
                ensure_diff_for_selected(app).await;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                app.diff_mut().set_focus(DiffFocus::Body);
            }
            _ => {}
        },
        DiffFocus::Body => match code {
            KeyCode::Down | KeyCode::Char('j') => app.diff_mut().body_scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => app.diff_mut().body_scroll_by(-1),
            KeyCode::PageDown => {
                let half = (app.diff().body_height.get() / 2).max(1) as i32;
                app.diff_mut().body_scroll_by(half);
            }
            KeyCode::PageUp => {
                let half = (app.diff().body_height.get() / 2).max(1) as i32;
                app.diff_mut().body_scroll_by(-half);
            }
            KeyCode::Home => app.diff_mut().body_scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => app.diff_mut().body_scroll_to_bottom(),
            KeyCode::Char('g') if app.diff_mut().consume_g_chord() => {
                app.diff_mut().body_scroll_to_top();
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                app.diff_mut().set_focus(DiffFocus::Files);
            }
            _ => {}
        },
    }
}
