//! Key dispatch for the args-prompt modal.
//!
//! Mouse events fall through to the parent view because the args
//! prompt is single-input — there's no list to click on.

use crate::tui::app::{App, LaunchRejection};
use crate::tui::terminal::launch_message;
use crossterm::event::{KeyCode, KeyModifiers};

pub fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // Ctrl-c always quits, even mid-prompt.
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    match code {
        KeyCode::Esc => {
            app.args_prompt_resolve(false);
        }
        KeyCode::Backspace => app.args_prompt_pop_char(),
        KeyCode::Enter => match app.args_prompt_resolve(true) {
            Some(Ok(())) => {}
            Some(Err(LaunchRejection::AlreadyRunning)) => {
                app.open_kill_restart_confirm();
            }
            Some(Err(rej)) => app.flash(launch_message(rej)),
            None => {}
        },
        KeyCode::Char(c) => app.args_prompt_push_char(c),
        _ => {}
    }
}
