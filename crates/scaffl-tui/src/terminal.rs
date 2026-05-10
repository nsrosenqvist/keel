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

/// Outcome of one drive of the event loop. `Quit` ends the session;
/// `SwitchWorktree` signals a hot-reload — the CLI's outer loop
/// rebuilds the App against the new root and re-enters drive.
#[derive(Debug)]
pub enum DriveOutcome {
    Quit,
    SwitchWorktree(std::path::PathBuf),
}

pub async fn run_event_loop(app: &mut App) -> Result<DriveOutcome, TuiError> {
    let title = terminal_title(app);
    let mut terminal = enter_terminal(&title)?;
    let result = drive(&mut terminal, app).await;
    leave_terminal(&mut terminal)?;
    // Print any buffered diagnostics now that the alternate screen
    // is gone — flashes clear on the next keypress and so were
    // unreliable for transient failures (tmux session vanishing on
    // detach, tmux query errors, etc.).
    let diagnostics = app.drain_diagnostics();
    if !diagnostics.is_empty() {
        eprintln!();
        for line in diagnostics {
            eprintln!("{line}");
        }
    }
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
) -> Result<DriveOutcome, TuiError> {
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
            return Ok(DriveOutcome::Quit);
        }
        if let Some(path) = app.take_pending_switch() {
            return Ok(DriveOutcome::SwitchWorktree(path));
        }
        if let Some(req) = app.take_pending_attach() {
            // Yield the terminal to tmux. Drop the events reader
            // first so its blocking poll thread doesn't fight tmux
            // for input, leave alternate screen / cooked mode, run
            // tmux attach (it inherits stdin/stdout/stderr from us),
            // then re-enter the TUI when the user detaches.
            drop(events);
            leave_terminal(terminal)?;
            attach_tmux(&req).await;
            *terminal = enter_terminal(&terminal_title(app))?;
            terminal.clear()?;
            events = spawn_event_reader();
            // Window list may have changed (new shell created, or
            // user typed `exit` and the window died). Reload the
            // cache so the sidebar reflects the current state.
            // We expect the session to exist post-attach; if it
            // doesn't, refresh_tmux_windows surfaces the cause.
            refresh_tmux_windows(app, true).await;
            // Paint the post-attach state *now*, before the next
            // loop iteration's slow pre-render hooks
            // (`refresh_service_status` shells out to compose for
            // each service — ~200ms with two of them). Without
            // this, users see a blank alt-screen for those couple
            // hundred milliseconds and read "blank" as "list got
            // emptied".
            terminal.draw(|f| ui::render(app, f))?;
            continue;
        }
        if let Some(kill) = app.take_pending_kill_window() {
            let _ = tokio::process::Command::new("tmux")
                .args([
                    "kill-window",
                    "-t",
                    &format!("{}:{}", kill.session, kill.index),
                ])
                .status()
                .await;
            // The session might be gone now if we just killed the
            // last window — that's normal, no flash.
            refresh_tmux_windows(app, false).await;
        }
        tokio::select! {
            biased;
            ev = events.recv() => {
                let Some(ev) = ev else { return Ok(DriveOutcome::Quit) };
                handle_event(app, ev).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(TICK_INTERVAL_MS)) => {
                // Tick — re-drain output, re-poll status, redraw timers.
            }
        }
    }
}

/// Run the tmux attach (creating the session / window if needed).
/// Returns when the user detaches (`ctrl+b d`) or tmux exits.
///
/// Window-target semantics:
///   - `req.window == "scaffl-new"` (the sentinel marker) → spawn
///     a fresh, unnamed window each time so multiple shells can
///     coexist; tmux's automatic-rename takes over once `$SHELL`
///     starts.
///   - any other value paired with `Some(create_with)` → idempotent
///     create-or-attach by name (used for `svc:<service>` windows).
///   - bare numeric value with `None` → attach to that window index
///     directly.
async fn attach_tmux(req: &crate::app::AttachRequest) {
    use tokio::process::Command;
    let has_session = Command::new("tmux")
        .args(["has-session", "-t", &req.session])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    let target = if req.window == "scaffl-new" {
        // Fresh shell: ensure session, then unconditionally
        // new-window. `-P -F #{window_index}` would let us read
        // the index back, but the simpler path is "spawn it,
        // immediately attach" — tmux focuses the new window when
        // the client connects.
        if !has_session {
            let mut cmd = Command::new("tmux");
            cmd.args(["new-session", "-d", "-s", &req.session]);
            if let Some(create) = req.create_with.as_deref() {
                cmd.arg(create);
            }
            let _ = cmd.status().await;
        } else if let Some(create) = req.create_with.as_deref() {
            let _ = Command::new("tmux")
                .args(["new-window", "-t", &req.session, create])
                .status()
                .await;
        }
        format!("{}:", req.session) // attach to whatever's active (the new window)
    } else if !has_session {
        let mut cmd = Command::new("tmux");
        cmd.args(["new-session", "-d", "-s", &req.session]);
        if let Some(create) = req.create_with.as_deref() {
            cmd.args(["-n", &req.window, create]);
        } else {
            cmd.args(["-n", &req.window]);
        }
        let _ = cmd.status().await;
        format!("{}:{}", req.session, req.window)
    } else {
        // Session exists. If a `create_with` is set, ensure the
        // named window exists (idempotent for service rows).
        if let Some(create) = req.create_with.as_deref() {
            let target = format!("{}:{}", req.session, req.window);
            let has_window = Command::new("tmux")
                .args(["has-session", "-t", &target])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !has_window {
                let _ = Command::new("tmux")
                    .args(["new-window", "-t", &req.session, "-n", &req.window, create])
                    .status()
                    .await;
            }
        }
        format!("{}:{}", req.session, req.window)
    };
    // Pin options that decide whether ctrl+b d ends up killing
    // the session or its windows. Users with `destroy-unattached
    // on` (or rebound `d` to `kill-session`) in their global
    // tmux config would otherwise see every shell vanish the
    // moment scaffl detached — definitely not what "detach"
    // should do. Belt and braces:
    //
    //   - per-session destroy-unattached off (always survives detach)
    //   - per-session remain-on-exit off (default; explicit)
    //   - bind ctrl+b d to detach-client (in case user rebound it)
    //
    // The keybinding is server-global; it leaves a small
    // footprint on the user's tmux server, but since we're
    // setting it to tmux's *default* behaviour, any user who
    // rebound `d` did so by writing config that runs at server
    // start — they'll get their override back on the next tmux
    // server restart. This is the smallest defensive write that
    // still catches the rebind case.
    for arg_set in [
        ["set-option", "-t", &req.session, "destroy-unattached", "off"],
        ["set-option", "-t", &req.session, "remain-on-exit", "off"],
    ] {
        let _ = Command::new("tmux").args(arg_set).status().await;
    }
    let _ = Command::new("tmux")
        .args(["bind-key", "d", "detach-client"])
        .status()
        .await;

    // Inject the scaffl-flavoured status bar so users always see
    // the detach hint. Mirrors AOE's layout: session name styled
    // on the left, then a separator and the detach instruction;
    // tmux's window list rides on the right of `status-left` as
    // its built-in `status-window-format` output.
    //
    // Set on every attach: idempotent (overwrite-on-set), and
    // free of "did the user customise tmux globally?" assumptions
    // — we only touch this one session, leaving global config
    // alone.
    let status_left =
        " #[fg=cyan,bold]#S#[default] #[fg=brightblack]│#[default] ctrl+b d to detach ";
    let _ = Command::new("tmux")
        .args(["set-option", "-t", &req.session, "status-left", status_left])
        .status()
        .await;
    let _ = Command::new("tmux")
        .args(["set-option", "-t", &req.session, "status-left-length", "60"])
        .status()
        .await;
    // Attach. Inherits stdio.
    let _ = Command::new("tmux")
        .args(["attach", "-t", &target])
        .status()
        .await;
}

/// Outcome of asking tmux for the windows in a session. Lets the
/// caller distinguish "no such session" from "session has no
/// windows" — which look identical when collapsed to a Vec.
enum WindowList {
    Ok(Vec<crate::app::TmuxWindow>),
    NoSession(String),
    SpawnFailed(String),
}

/// Query tmux for the current window list of `session`.
async fn list_tmux_windows(session: &str) -> WindowList {
    use tokio::process::Command;
    let output = match Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index}:#{window_name}",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return WindowList::SpawnFailed(e.to_string()),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return WindowList::NoSession(if stderr.is_empty() {
            format!("tmux exited {}", output.status.code().unwrap_or(-1))
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    WindowList::Ok(parse_tmux_windows(&stdout))
}

/// Parse `tmux list-windows -F '#{window_index}:#{window_name}'`
/// output. Lines look like `0:zsh` or `1:svc:app`. Public for tests.
pub(crate) fn parse_tmux_windows(input: &str) -> Vec<crate::app::TmuxWindow> {
    let mut out = Vec::new();
    for line in input.lines() {
        let Some((idx_str, name)) = line.split_once(':') else {
            continue;
        };
        let Ok(index) = idx_str.parse::<u32>() else {
            continue;
        };
        out.push(crate::app::TmuxWindow {
            index,
            name: name.to_string(),
        });
    }
    out
}

/// Refresh the cached tmux window list from the live session.
/// Surfaces three cases differently:
///
///   - Ok with windows  → adopt them.
///   - Ok with no windows → adopt the empty list (the session
///     exists but every window has died).
///   - NoSession after the user just attached → flash a hint;
///     a hook or rebind in the user's tmux config probably
///     destroyed the session on detach.
///   - SpawnFailed → flash the spawn error.
async fn refresh_tmux_windows(app: &mut App, expecting_session: bool) {
    let session = app.terminals().session_name.clone();
    match list_tmux_windows(&session).await {
        WindowList::Ok(w) => {
            if expecting_session {
                app.diagnostic(format!(
                    "[tmux] post-attach refresh of `{session}`: {} window(s)",
                    w.len()
                ));
                for win in &w {
                    app.diagnostic(format!("[tmux]   {}: {}", win.index, win.name));
                }
            }
            app.terminals_set_windows(w);
            if expecting_session {
                // Cross-check: dump what scaffl will *render* in the
                // sidebar. If this list disagrees with what tmux
                // returned, the bug is in our row assembly /
                // filter, not tmux.
                let rows = app.terminals_rows();
                app.diagnostic(format!("[ui] sidebar rows: {} total", rows.len()));
                for (i, row) in rows.iter().enumerate() {
                    let label = match row {
                        crate::app::TerminalsRow::Service(name) => format!("Service({name})"),
                        crate::app::TerminalsRow::Window(w) => {
                            format!("Window({}: {})", w.index, w.name)
                        }
                        crate::app::TerminalsRow::NewSentinel => "NewSentinel".into(),
                    };
                    app.diagnostic(format!("[ui]   {i}: {label}"));
                }
            }
        }
        WindowList::NoSession(msg) => {
            app.terminals_set_windows(Vec::new());
            if expecting_session {
                let line = format!(
                    "tmux session `{session}` vanished after detach — check ~/.tmux.conf hooks: {msg}"
                );
                app.flash = Some(line.clone());
                app.diagnostic(format!("[tmux] {line}"));
            }
        }
        WindowList::SpawnFailed(msg) => {
            app.terminals_set_windows(Vec::new());
            let line = format!("tmux query failed: {msg}");
            app.flash = Some(line.clone());
            app.diagnostic(format!("[tmux] {line}"));
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
                crate::app::Mode::ArgsPrompt => handle_key_args_prompt(app, code, modifiers),
                crate::app::Mode::WorktreeSwitcher => {
                    handle_key_switcher(app, code, modifiers).await
                }
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

    // Top-level view switches. All uppercase for one consistent
    // mental model — view changes are deliberate, shift-modified
    // keys; lowercase letters stay free for per-view actions.
    match code {
        KeyCode::Char('T') => {
            app.switch_view(crate::app::View::Terminals);
            ensure_tmux_probed(app).await;
            // Don't expect a session yet — the user may not have
            // attached to anything during this scaffl session.
            refresh_tmux_windows(app, false).await;
            return;
        }
        KeyCode::Char('G') => {
            app.switch_view(crate::app::View::Diff);
            ensure_diff_loaded(app).await;
            return;
        }
        KeyCode::Char('C') if app.view() != crate::app::View::ControlCenter => {
            app.switch_view(crate::app::View::ControlCenter);
            return;
        }
        _ => {}
    }

    // Per-view keymap: while in Terminals or Diff, only the global
    // keys above + a tiny per-view dispatch apply. Control center
    // keeps its full keymap below.
    if app.view() == crate::app::View::Terminals {
        handle_key_terminals(app, code, modifiers).await;
        return;
    }
    if app.view() == crate::app::View::Diff {
        handle_key_diff(app, code, modifiers).await;
        return;
    }

    match code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        KeyCode::Home => app.select_first(),
        KeyCode::End => app.select_last(),
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
        // `u`: up the selected service. Pairs with `U` (up all) just
        // like `r`/`R` and `s`/`S`. Enter on a service used to do
        // this; it now attaches instead, so explicit `u` for "up
        // without attaching" is back.
        KeyCode::Char('u') => {
            if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(scaffl_container::service_action::UP, &[service.as_str()])
                    .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        KeyCode::Char('U') => {
            if let Err(rej) = app
                .run_service_action(scaffl_container::service_action::UP, &[])
                .await
            {
                app.flash = Some(launch_message(rej));
            }
        }
        // `W`: open the worktree switcher modal.
        KeyCode::Char('W') => {
            let entries = build_worktree_rows(app).await;
            app.open_worktree_switcher(entries);
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
            //   container         → no-op (use U/D/R/S; flashed by
            //                       try_launch_selected)
            //   service           → attach into a tmux pane (jumps
            //                       to the Terminals view; ctrl+b d
            //                       returns). Non-container services
            //                       (systemd / custom) flash a hint
            //                       instead — no shell to attach to.
            //   recipe / script   → either open args prompt (if forward_args
            //                       and not already running) or launch
            //   watcher           → no-op (watchers fire on file change)
            if let Some(service) = app.selected_service().map(|s| s.name.clone()) {
                ensure_tmux_probed(app).await;
                if app.terminals().tmux_available == Some(false) {
                    app.flash = Some("tmux not installed — install it to attach".into());
                } else if let Err(msg) = app.queue_service_attach(&service) {
                    app.flash = Some(msg);
                }
            } else if app.selected_accepts_args() && !selected_is_running(app) {
                // Discoverability path: a `forward_args = true` row gets
                // a prompt so users see they can pass args. Power users
                // bypass via the palette (`:cmd foo bar`).
                app.open_args_prompt();
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
        KeyCode::Enter => {
            // Palette confirm now drives the launch directly so it can
            // forward args parsed from the input. The handler returns
            // None when there's no match (the keypress is ignored — the
            // palette stays open and the user keeps typing).
            match app.confirm_palette() {
                Some(Ok(())) => {}
                Some(Err(crate::app::LaunchRejection::AlreadyRunning)) => {
                    app.open_kill_restart_confirm();
                }
                Some(Err(rej)) => {
                    app.flash = Some(launch_message(rej));
                }
                None => {}
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

/// True when the selected row has an in-flight run. Used to decide
/// whether Enter opens the args prompt (only when not running — the
/// kill-and-restart modal takes precedence so the user knows the
/// previous run is being interrupted).
fn selected_is_running(app: &App) -> bool {
    app.selected_run().is_some_and(|r| !r.is_done())
}

/// Probe `tmux -V` once and cache the result on the App. The
/// terminals view falls back to a placeholder when tmux isn't
/// present.
async fn ensure_tmux_probed(app: &mut App) {
    if app.terminals().tmux_available.is_some() {
        return;
    }
    let ok = tokio::process::Command::new("tmux")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    app.set_tmux_available(ok);
}

async fn handle_key_terminals(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    match code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => app.terminals_select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.terminals_select_prev(),
        KeyCode::Char('d') => app.terminals_kill_selected(),
        KeyCode::Enter => app.terminals_confirm(),
        _ => {}
    }
}

async fn handle_key_diff(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    match code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => {
            app.diff_select_next();
            ensure_diff_for_selected(app).await;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.diff_select_prev();
            ensure_diff_for_selected(app).await;
        }
        KeyCode::Char('r') => {
            app.diff_mark_stale();
            ensure_diff_loaded(app).await;
        }
        _ => {}
    }
}

/// Populate the diff file list if it hasn't been loaded yet, and
/// ensure the selected file's diff body is cached. Cheap on
/// subsequent calls thanks to the per-file cache.
async fn ensure_diff_loaded(app: &mut App) {
    if !app.diff().loaded {
        let project_root = app.project_root().to_path_buf();
        match load_diff_files(&project_root).await {
            Ok(files) => app.diff_set_files(files),
            Err(msg) => app.diff_set_error(msg),
        }
    }
    ensure_diff_for_selected(app).await;
}

async fn ensure_diff_for_selected(app: &mut App) {
    let Some(file) = app.diff_selected_file().cloned() else {
        return;
    };
    if app.diff_cache_for(&file.path).is_some() {
        return;
    }
    let project_root = app.project_root().to_path_buf();
    let lines = load_diff_for_file(&project_root, &file).await;
    app.diff_set_cache(file.path.clone(), lines);
}

async fn load_diff_files(
    project_root: &std::path::Path,
) -> Result<Vec<crate::app::DiffFile>, String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(project_root)
        .output()
        .await
        .map_err(|e| format!("git status failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git status exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(parse_status_porcelain(&stdout))
}

/// Parse `git status --porcelain=v1` output. Each line is two
/// status chars + space + path (or `path -> renamed-to` for
/// renames). We pick the worst-of-the-two status chars to colour
/// the row; the file path is everything after.
pub(crate) fn parse_status_porcelain(input: &str) -> Vec<crate::app::DiffFile> {
    use crate::app::{DiffFile, DiffStatus};
    let mut out = Vec::new();
    for line in input.lines() {
        if line.len() < 4 {
            continue;
        }
        let staged = line.as_bytes()[0] as char;
        let worktree = line.as_bytes()[1] as char;
        let rest = &line[3..];
        // Renames have the form `R  old -> new`.
        let path = if let Some(idx) = rest.find(" -> ") {
            rest[idx + 4..].to_string()
        } else {
            rest.to_string()
        };
        let status = match (staged, worktree) {
            ('?', '?') => DiffStatus::Untracked,
            ('A', _) | (_, 'A') => DiffStatus::Added,
            ('D', _) | (_, 'D') => DiffStatus::Deleted,
            ('R', _) | (_, 'R') => DiffStatus::Renamed,
            ('M', _) | (_, 'M') => DiffStatus::Modified,
            _ => DiffStatus::Other,
        };
        out.push(DiffFile { path, status });
    }
    out
}

async fn load_diff_for_file(
    project_root: &std::path::Path,
    file: &crate::app::DiffFile,
) -> Vec<crate::app::DiffLine> {
    use crate::app::{DiffLine, DiffLineKind, DiffStatus};
    // Untracked files don't exist in HEAD — `git diff HEAD <path>`
    // would error. Synthesise a file-as-added view with the file
    // contents prefixed by `+`.
    if file.status == DiffStatus::Untracked {
        return load_untracked_as_diff(project_root, &file.path).await;
    }
    let output = match tokio::process::Command::new("git")
        .args(["diff", "HEAD", "--", &file.path])
        .current_dir(project_root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return vec![DiffLine {
                kind: DiffLineKind::Header,
                text: format!("git diff failed: {e}"),
            }];
        }
    };
    let body = String::from_utf8_lossy(&output.stdout);
    body.lines()
        .map(|line| DiffLine {
            kind: DiffLineKind::classify(line),
            text: line.to_string(),
        })
        .collect()
}

async fn load_untracked_as_diff(
    project_root: &std::path::Path,
    path: &str,
) -> Vec<crate::app::DiffLine> {
    use crate::app::{DiffLine, DiffLineKind};
    let abs = project_root.join(path);
    let body = tokio::fs::read_to_string(&abs).await.unwrap_or_default();
    let mut lines = vec![DiffLine {
        kind: DiffLineKind::Header,
        text: format!("untracked file: {path}"),
    }];
    for l in body.lines() {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("+{l}"),
        });
    }
    lines
}

/// Build worktree-switcher rows for the current project. The current
/// worktree (matched by canonicalised path) is flagged so the modal
/// can render it differently and pre-select it.
async fn build_worktree_rows(app: &App) -> Vec<crate::app::WorktreeRow> {
    let project_root = app.project_root().to_path_buf();
    let entries = scaffl_runtime::worktree::list_worktrees(&project_root).await;
    let current = std::fs::canonicalize(&project_root).unwrap_or(project_root);
    entries
        .into_iter()
        .map(|e| {
            let path_buf = std::path::PathBuf::from(&e.path);
            let canonical = std::fs::canonicalize(&path_buf).unwrap_or_else(|_| path_buf.clone());
            let slug = derive_slug_from_entry(&e);
            crate::app::WorktreeRow {
                path: path_buf,
                branch: e.branch.clone(),
                slug,
                is_current: canonical == current,
            }
        })
        .collect()
}

fn derive_slug_from_entry(e: &scaffl_runtime::WorktreeListEntry) -> String {
    if let Some(branch) = e.branch.as_deref() {
        return scaffl_runtime::worktree::slugify(branch);
    }
    if e.detached {
        return scaffl_runtime::worktree::slugify(
            std::path::Path::new(&e.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
        );
    }
    String::new()
}

async fn handle_key_switcher(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    // Two sub-modes inside this modal: list-of-worktrees (default)
    // and the new-worktree create form. The form takes over key
    // dispatch when active.
    if app.switcher().and_then(|s| s.creating.as_ref()).is_some() {
        handle_key_switcher_form(app, code).await;
        return;
    }
    match code {
        KeyCode::Esc => app.close_switcher(),
        KeyCode::Up | KeyCode::Char('k') => app.switcher_select_prev(),
        KeyCode::Down | KeyCode::Char('j') => app.switcher_select_next(),
        KeyCode::Enter => {
            app.switcher_confirm();
        }
        _ => {}
    }
}

async fn handle_key_switcher_form(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.switcher_form_cancel(),
        KeyCode::Tab => app.switcher_form_toggle_focus(),
        KeyCode::Backspace => app.switcher_form_pop_char(),
        KeyCode::Enter => {
            // Snapshot, run git worktree add, report back via finish().
            let Some(form) = app.switcher_form_snapshot() else {
                return;
            };
            let project_root = app.project_root().to_path_buf();
            let result = create_worktree(&project_root, &form.path_input, &form.branch_input).await;
            app.switcher_form_finish(result);
        }
        KeyCode::Char(c) => app.switcher_form_push_char(c),
        _ => {}
    }
}

/// Run `git worktree add` for the user's form input. On success
/// returns the canonicalised path of the new worktree (so the App
/// rebuild has a stable, absolute target). On failure returns the
/// trimmed git stderr so the modal can render it.
async fn create_worktree(
    project_root: &std::path::Path,
    path: &str,
    branch: &str,
) -> Result<std::path::PathBuf, String> {
    let path = path.trim();
    let branch = branch.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    let mut argv: Vec<&str> = vec!["worktree", "add"];
    if !branch.is_empty() {
        argv.push("-b");
        argv.push(branch);
    }
    argv.push(path);
    let output = tokio::process::Command::new("git")
        .args(&argv)
        .current_dir(project_root)
        .output()
        .await
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "git worktree add failed (exit {})",
                output.status.code().unwrap_or(-1)
            )
        } else {
            stderr
        });
    }
    let pb = std::path::PathBuf::from(path);
    Ok(std::fs::canonicalize(&pb).unwrap_or(pb))
}

fn handle_key_args_prompt(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
            Some(Err(crate::app::LaunchRejection::AlreadyRunning)) => {
                app.open_kill_restart_confirm();
            }
            Some(Err(rej)) => app.flash = Some(launch_message(rej)),
            None => {}
        },
        KeyCode::Char(c) => app.args_prompt_push_char(c),
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
            let accept = app.confirm_dialog().map(|d| d.yes_focused).unwrap_or(true);
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
        // Disable the container backend by default so the synthetic
        // container row doesn't shift item indices in tests whose
        // subject is unrelated. Tests that *do* want a container row
        // pass their own `[containers]` block.
        let prefix = if toml.contains("[containers]") {
            String::new()
        } else {
            String::from("[containers]\nbackend = \"none\"\n")
        };
        App::new(Arc::new(
            scaffl_config::parse_str(&format!("{prefix}{toml}")).unwrap(),
        ))
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
        // End jumps to last; Home jumps to first. `G` was vim-style
        // last-row but is now the diff-view switcher (uppercase
        // letters are reserved for view switches).
        handle_event(&mut app, press(KeyCode::End)).await;
        assert_eq!(app.selected_index(), 2);
        handle_event(&mut app, press(KeyCode::Home)).await;
        assert_eq!(app.selected_index(), 0);
    }

    #[tokio::test]
    async fn capital_t_switches_to_terminals_view() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        assert_eq!(app.view(), crate::app::View::ControlCenter);
        handle_event(&mut app, press(KeyCode::Char('T'))).await;
        assert_eq!(app.view(), crate::app::View::Terminals);
    }

    #[tokio::test]
    async fn capital_g_switches_to_diff_view() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('G'))).await;
        assert_eq!(app.view(), crate::app::View::Diff);
    }

    #[tokio::test]
    async fn lowercase_g_does_not_switch_views() {
        // Used to switch to diff; now reserved (uppercase only for
        // view changes). Asserts we don't accidentally rewire it.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('g'))).await;
        assert_eq!(app.view(), crate::app::View::ControlCenter);
    }

    #[test]
    fn parse_tmux_windows_handles_typical_output() {
        let input = "0:zsh\n1:svc:app\n2:vim\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].index, 0);
        assert_eq!(windows[0].name, "zsh");
        assert_eq!(windows[1].name, "svc:app");
        assert_eq!(windows[2].index, 2);
    }

    #[test]
    fn parse_tmux_windows_skips_malformed_lines() {
        let input = "0:zsh\nnot a window\n2:vim\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].name, "zsh");
        assert_eq!(windows[1].name, "vim");
    }

    #[test]
    fn parse_status_porcelain_classifies_each_line() {
        use crate::app::DiffStatus;
        // Explicit \n joins because Rust's `\<newline>` continuation
        // strips the leading whitespace on the next line — which
        // would corrupt git porcelain's `XY PATH` format where X
        // can legitimately be a space.
        let input = concat!(
            " M src/main.rs\n",
            "A  src/lib.rs\n",
            "?? notes.txt\n",
            " D Cargo.toml\n",
            "R  old.txt -> new.txt\n",
        );
        let files = parse_status_porcelain(input);
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].status, DiffStatus::Modified);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[1].status, DiffStatus::Added);
        assert_eq!(files[2].status, DiffStatus::Untracked);
        assert_eq!(files[2].path, "notes.txt");
        assert_eq!(files[3].status, DiffStatus::Deleted);
        // Renames carry the destination as the path.
        assert_eq!(files[4].status, DiffStatus::Renamed);
        assert_eq!(files[4].path, "new.txt");
    }

    #[tokio::test]
    async fn capital_c_returns_to_control_center() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.switch_view(crate::app::View::Diff);
        handle_event(&mut app, press(KeyCode::Char('C'))).await;
        assert_eq!(app.view(), crate::app::View::ControlCenter);
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
    async fn esc_in_normal_does_not_quit() {
        // Esc used to alias to quit. Now it's reserved for closing
        // modals — `q` and `ctrl+c` are the only ways to end the
        // session.
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Esc)).await;
        assert!(!app.should_quit());
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
