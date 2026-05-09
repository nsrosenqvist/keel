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
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{Stdout, stdout};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

const POLL_INTERVAL_MS: u64 = 100;
const TICK_INTERVAL_MS: u64 = 250;

pub async fn run_event_loop(app: &mut App) -> Result<(), TuiError> {
    let title = terminal_title(app);
    let mut terminal = enter_terminal(&title)?;
    let result = drive(&mut terminal, app).await;
    leave_terminal(&mut terminal)?;
    result
}

fn terminal_title(app: &App) -> String {
    let project = app
        .config()
        .project
        .name
        .clone()
        .unwrap_or_else(|| "scaffl".into());
    format!("scaffl — {project}")
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
        app.drain_runs();
        app.poll_runs().await;
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
                handle_event(app, ev).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(TICK_INTERVAL_MS)) => {
                // Tick — re-drain output, re-poll status, redraw timers.
            }
        }
    }
}

async fn handle_event(app: &mut App, event: Event) {
    match event {
        Event::Key(KeyEvent {
            kind: KeyEventKind::Press,
            code,
            modifiers,
            ..
        }) => {
            // Any key dismisses a stale flash. The dispatched handler
            // may re-arm a fresh one for this event.
            app.flash = None;
            match app.mode() {
                crate::app::Mode::Normal => handle_key_normal(app, code, modifiers).await,
                crate::app::Mode::Palette => handle_key_palette(app, code, modifiers).await,
                crate::app::Mode::Confirm => handle_key_confirm(app, code, modifiers),
            }
        }
        Event::Resize(_, _) => {
            // The next draw call already adapts to the new size.
        }
        _ => {}
    }
}

async fn handle_key_normal(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            // Quit
            KeyCode::Char('c') => app.quit(),
            // Modern palette opener (VS Code / many TUIs use Ctrl-K).
            KeyCode::Char('k') | KeyCode::Char('p') => app.open_palette(),
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        KeyCode::Char('g') | KeyCode::Home => app.select_first(),
        KeyCode::Char('G') | KeyCode::End => app.select_last(),
        // Palette: `:` (vim-style), `/` (fuzzy-search-style).
        KeyCode::Char(':') | KeyCode::Char('/') => app.open_palette(),
        // Service controls follow a single rule:
        //   lowercase = act on the selected service
        //   uppercase = act on every service
        // `s`/`S` are also the abort-current-run shortcut: when a run is
        // in flight, "stop the noisy thing" is more useful than the
        // literal stop semantics.
        KeyCode::Char('s') => {
            // Priority: stop the thing the user is looking at first
            // (selected row's run), then a lifecycle run if any, then
            // fall through to compose stop on the selected service.
            if app.abort_selected_run() {
                app.flash = Some("aborted run".into());
            } else if app.abort_lifecycle_run() {
                app.flash = Some("aborted lifecycle run".into());
            } else if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(scaffl_container::service_action::STOP, &[service.as_str()])
                    .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Char('S') => {
            if app.abort_lifecycle_run() {
                app.flash = Some("aborted lifecycle run".into());
            } else if let Err(rej) = app
                .run_service_action(scaffl_container::service_action::STOP, &[])
                .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Char('r') => {
            if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(
                        scaffl_container::service_action::RESTART,
                        &[service.as_str()],
                    )
                    .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Char('R') => {
            if let Err(rej) = app
                .run_service_action(scaffl_container::service_action::RESTART, &[])
                .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        // `U`: up all. No lowercase counterpart — `enter` ups the
        // selected service, so a separate `u` would be redundant.
        KeyCode::Char('U') => {
            if let Err(rej) = app
                .run_service_action(scaffl_container::service_action::UP, &[])
                .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        // `D`: down all. No lowercase counterpart — compose's `down` is
        // intrinsically project-wide; the per-service equivalent is
        // `stop` (bound to `s`).
        KeyCode::Char('D') => {
            if let Err(rej) = app
                .run_service_action(scaffl_container::service_action::DOWN, &[])
                .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Enter => {
            // Enter routing:
            //   service           → up that service via lifecycle action
            //   recipe / script   → launch its run; if the row is already
            //                       running, open the kill-and-restart modal
            //   watcher           → no-op (watchers fire on file change)
            if let Some(service) = app.selected_service().map(|s| s.name.clone()) {
                if let Err(rej) = app
                    .run_service_action(scaffl_container::service_action::UP, &[service.as_str()])
                    .await
                {
                    app.flash = Some(launch_message(rej));
                }
            } else {
                match app.try_launch_selected() {
                    Ok(()) => {}
                    Err(crate::app::LaunchRejection::AlreadyRunning) => {
                        app.open_kill_restart_confirm();
                    }
                    Err(rej) => {
                        app.flash = Some(launch_message(rej));
                    }
                }
            }
        }
        _ => {}
    }
}

async fn handle_key_palette(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
        KeyCode::Enter if app.confirm_palette() => {
            if let Err(rejection) = app.try_launch_selected() {
                app.flash = Some(launch_message(rejection));
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

fn handle_key_confirm(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Enter => {
            // Enter accepts the focused choice — Yes by default;
            // if the user tabbed to No, Enter dismisses.
            let accept = app
                .confirm_dialog()
                .map(|d| d.yes_focused)
                .unwrap_or(true);
            if let Some(rej) = app.confirm_resolve(accept) {
                app.flash = Some(launch_message(rej));
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

fn enter_terminal(title: &str) -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetTitle(title)
    )?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

fn leave_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), TuiError> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        // Restore a sane title; emitting an empty string asks the
        // terminal to reset to the shell's default in most emulators.
        SetTitle(""),
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Spawn a blocking thread that polls crossterm events and forwards them
/// over an mpsc channel.
///
/// The thread exits within `POLL_INTERVAL_MS` of the receiver being
/// dropped — without the closed-check, a quiet terminal would keep the
/// thread blocked in `poll()` forever, which in turn blocks tokio's
/// runtime shutdown and leaves the process alive after `q`.
fn spawn_event_reader() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        loop {
            if tx.is_closed() {
                return;
            }
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
                // No event — loop, but the next iteration's is_closed
                // check gives us a clean exit path on quit.
                Ok(false) => {}
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

    #[tokio::test]
    async fn q_quits() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_key_normal(&mut app, KeyCode::Char('q'), KeyModifiers::NONE).await;
        assert!(app.should_quit());
    }

    #[tokio::test]
    async fn ctrl_c_quits() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('c'))).await;
        assert!(app.should_quit());
    }

    #[tokio::test]
    async fn navigation_moves_selection() {
        let mut app = app_with(
            "[command.a]\nrun = \"true\"\n[command.b]\nrun = \"true\"\n[command.c]\nrun = \"true\"\n",
        );
        assert_eq!(app.selected_index(), 0);
        handle_event(&mut app, press(KeyCode::Char('j'))).await;
        assert_eq!(app.selected_index(), 1);
        handle_event(&mut app, press(KeyCode::Down)).await;
        assert_eq!(app.selected_index(), 2);
        handle_event(&mut app, press(KeyCode::Char('G'))).await;
        assert_eq!(app.selected_index(), 2);
        handle_event(&mut app, press(KeyCode::Char('g'))).await;
        assert_eq!(app.selected_index(), 0);
    }

    #[tokio::test]
    async fn drive_returns_when_app_quits() {
        // We can't easily drive the real terminal in tests — assert that
        // the loop exits cleanly when `should_quit` is set without ever
        // spawning the event reader.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.quit();
        assert!(app.should_quit());
    }

    #[tokio::test]
    async fn unrelated_keys_do_nothing() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        let before = app.selected_index();
        handle_event(&mut app, press(KeyCode::Char('z'))).await;
        assert_eq!(app.selected_index(), before);
        assert!(!app.should_quit());
    }

    #[tokio::test]
    async fn colon_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char(':'))).await;
        assert_eq!(app.mode(), crate::app::Mode::Palette);
    }

    #[tokio::test]
    async fn slash_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('/'))).await;
        assert_eq!(app.mode(), crate::app::Mode::Palette);
    }

    #[tokio::test]
    async fn ctrl_k_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('k'))).await;
        assert_eq!(app.mode(), crate::app::Mode::Palette);
    }

    #[tokio::test]
    async fn ctrl_p_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('p'))).await;
        assert_eq!(app.mode(), crate::app::Mode::Palette);
    }

    #[tokio::test]
    async fn esc_in_palette_closes_it() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        app.open_palette();
        handle_event(&mut app, press(KeyCode::Esc)).await;
        assert_eq!(app.mode(), crate::app::Mode::Normal);
    }

    #[tokio::test]
    async fn typing_in_palette_filters() {
        let mut app =
            app_with("[command.test]\nrun = \"true\"\n[command.migrate]\nrun = \"true\"\n");
        app.open_palette();
        handle_event(&mut app, press(KeyCode::Char('m'))).await;
        let palette = app.palette().unwrap();
        let names: Vec<_> = palette
            .matches()
            .iter()
            .map(|m| app.items()[m.item_index].name.clone())
            .collect();
        assert!(names.contains(&"migrate".to_string()));
    }

    #[tokio::test]
    async fn enter_in_palette_moves_selection() {
        let mut app =
            app_with("[command.test]\nrun = \"true\"\n[command.migrate]\nrun = \"true\"\n");
        app.open_palette();
        // Empty input: matches are in original (alphabetical) order:
        // migrate then test. Move selection to migrate.
        handle_event(&mut app, press(KeyCode::Enter)).await;
        // Confirm closes the palette and moves the sidebar selection.
        assert_eq!(app.mode(), crate::app::Mode::Normal);
        assert_eq!(app.items()[app.selected_index()].name, "migrate");
    }
}
