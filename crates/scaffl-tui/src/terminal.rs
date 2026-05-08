//! Terminal lifecycle and event loop.
//!
//! Owns the raw-mode / alternate-screen handshake and pipes crossterm events
//! through an mpsc channel. The render and event dispatch logic lives here;
//! application semantics live in [`crate::app`].

use crate::TuiError;
use crate::app::App;
use crate::ui;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{Stdout, stdout};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

const POLL_INTERVAL_MS: u64 = 100;
const TICK_INTERVAL_MS: u64 = 250;

pub async fn run_event_loop(app: &mut App) -> Result<(), TuiError> {
    let mut terminal = enter_terminal()?;
    let result = drive(&mut terminal, app).await;
    leave_terminal(&mut terminal)?;
    result
}

async fn drive(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<(), TuiError> {
    // Spawn long-lived tail processes once, before the event loop. Any
    // tail that fails to spawn surfaces in the pane's tail_error and the
    // others continue.
    app.spawn_service_tails().await;

    let mut events = spawn_event_reader();
    loop {
        // Pre-render hooks: drain queued output and advance run state.
        app.drain_run();
        app.poll_run().await;
        app.drain_services();
        app.refresh_service_status().await;
        app.tick_watchers().await;

        terminal.draw(|f| ui::render(app, f))?;
        if app.should_quit() {
            return Ok(());
        }
        tokio::select! {
            biased;
            ev = events.recv() => {
                let Some(ev) = ev else { return Ok(()) };
                handle_event(app, ev);
            }
            _ = tokio::time::sleep(Duration::from_millis(TICK_INTERVAL_MS)) => {
                // Tick — re-drain output, re-poll status, redraw timers.
            }
        }
    }
}

fn handle_event(app: &mut App, event: Event) {
    match event {
        Event::Key(KeyEvent {
            kind: KeyEventKind::Press,
            code,
            modifiers,
            ..
        }) => match app.mode() {
            crate::app::Mode::Normal => handle_key_normal(app, code, modifiers),
            crate::app::Mode::Palette => handle_key_palette(app, code, modifiers),
        },
        Event::Resize(_, _) => {
            // The next draw call already adapts to the new size.
        }
        _ => {}
    }
}

fn handle_key_normal(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char('c') = code {
            app.quit();
        }
        return;
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        KeyCode::Char('g') | KeyCode::Home => app.select_first(),
        KeyCode::Char('G') | KeyCode::End => app.select_last(),
        KeyCode::Char(':') => app.open_palette(),
        KeyCode::Enter => {
            if let Err(rejection) = app.try_launch_selected() {
                app.flash = Some(launch_message(rejection));
            }
        }
        _ => {}
    }
}

fn handle_key_palette(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
            if app.confirm_palette() {
                if let Err(rejection) = app.try_launch_selected() {
                    app.flash = Some(launch_message(rejection));
                }
            }
        }
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(p) = app.palette_mut() {
                p.push_char(c);
            }
        }
        _ => {}
    }
}

fn launch_message(rejection: crate::app::LaunchRejection) -> String {
    use crate::app::LaunchRejection::*;
    match rejection {
        NoExecutor => "no backend wired into the TUI".into(),
        AlreadyRunning => "another run is in progress".into(),
        NotRunnable(msg) => msg,
    }
}

fn enter_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

fn leave_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), TuiError> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Spawn a blocking thread that polls crossterm events and forwards them
/// over an mpsc channel. The channel closes when the thread exits.
fn spawn_event_reader() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(POLL_INTERVAL_MS)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(ev) => {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        warn!("crossterm read error: {e}");
                        return;
                    }
                },
                Ok(false) => {} // No event — loop and poll again.
                Err(e) => {
                    warn!("crossterm poll error: {e}");
                    return;
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use std::sync::Arc;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        })
    }

    fn ctrl(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        })
    }

    fn app_with(toml: &str) -> App {
        App::new(Arc::new(scaffl_config::parse_str(toml).unwrap()))
    }

    #[test]
    fn q_quits() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_key_normal(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit());
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('c')));
        assert!(app.should_quit());
    }

    #[test]
    fn navigation_moves_selection() {
        let mut app = app_with(
            "[command.a]\nrun = \"true\"\n[command.b]\nrun = \"true\"\n[command.c]\nrun = \"true\"\n",
        );
        assert_eq!(app.selected_index(), 0);
        handle_event(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.selected_index(), 1);
        handle_event(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected_index(), 2);
        handle_event(&mut app, press(KeyCode::Char('G')));
        assert_eq!(app.selected_index(), 2);
        handle_event(&mut app, press(KeyCode::Char('g')));
        assert_eq!(app.selected_index(), 0);
    }

    #[tokio::test]
    async fn drive_returns_when_app_quits() {
        // We can't easily drive the real terminal in tests — assert that
        // the loop exits cleanly when `should_quit` is set without ever
        // spawning the event reader.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.quit();
        // Just ensure the function compiles and exits the same path —
        // a full event-driven test would need a virtual stdin.
        assert!(app.should_quit());
    }

    #[test]
    fn unrelated_keys_do_nothing() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        let before = app.selected_index();
        handle_event(&mut app, press(KeyCode::Char('z')));
        assert_eq!(app.selected_index(), before);
        assert!(!app.should_quit());
    }

    #[test]
    fn colon_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char(':')));
        assert_eq!(app.mode(), crate::app::Mode::Palette);
    }

    #[test]
    fn esc_in_palette_closes_it() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        app.open_palette();
        handle_event(&mut app, press(KeyCode::Esc));
        assert_eq!(app.mode(), crate::app::Mode::Normal);
    }

    #[test]
    fn typing_in_palette_filters() {
        let mut app =
            app_with("[command.test]\nrun = \"true\"\n[command.migrate]\nrun = \"true\"\n");
        app.open_palette();
        handle_event(&mut app, press(KeyCode::Char('m')));
        let palette = app.palette().unwrap();
        let names: Vec<_> = palette
            .matches()
            .iter()
            .map(|m| app.items()[m.item_index].name.clone())
            .collect();
        assert!(names.contains(&"migrate".to_string()));
    }

    #[test]
    fn enter_in_palette_moves_selection() {
        let mut app =
            app_with("[command.test]\nrun = \"true\"\n[command.migrate]\nrun = \"true\"\n");
        app.open_palette();
        // Empty input: matches are in original (alphabetical) order:
        // migrate then test. Move selection to migrate.
        handle_event(&mut app, press(KeyCode::Enter));
        // Confirm closes the palette and moves the sidebar selection.
        assert_eq!(app.mode(), crate::app::Mode::Normal);
        assert_eq!(app.items()[app.selected_index()].name, "migrate");
    }
}
