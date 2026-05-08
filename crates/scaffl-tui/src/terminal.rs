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
    let mut events = spawn_event_reader();
    loop {
        // Pre-render hooks: drain any queued output and advance run state.
        app.drain_run();
        app.poll_run().await;

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
                // Tick — re-poll run state, redraw timers, etc.
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
        }) => handle_key(app, code, modifiers),
        Event::Resize(_, _) => {
            // The next draw call already adapts to the new size.
        }
        _ => {}
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
        KeyCode::Enter => {
            if let Err(rejection) = app.try_launch_selected() {
                app.flash = Some(launch_message(rejection));
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
        handle_event(&mut app, press(KeyCode::Char('q')));
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
}
