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
/// rebuilds the App against the new root and re-enters drive,
/// carrying over the active view so the user lands where they left.
#[derive(Debug)]
pub enum DriveOutcome {
    Quit,
    SwitchWorktree {
        path: std::path::PathBuf,
        view: crate::app::View,
    },
}

pub async fn run_event_loop(
    app: &mut App,
    initial_view: crate::app::View,
) -> Result<DriveOutcome, TuiError> {
    let title = terminal_title(app);
    let mut terminal = enter_terminal(&title)?;
    // Run the same view-entry hooks the keymap fires when switching
    // views interactively — so a worktree hot-reload that lands in
    // Terminals or Diff doesn't show stale (empty) state on first
    // paint.
    match initial_view {
        crate::app::View::Terminals => {
            ensure_tmux_probed(app).await;
            refresh_tmux_windows(app, false).await;
        }
        crate::app::View::Diff => {
            ensure_diff_loaded(app).await;
        }
        crate::app::View::ControlCenter => {}
    }
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
        // All blocking I/O lives off this path — boot tasks deliver
        // their results via channels (drain_boot_results), the
        // worker polls service status on its own cadence
        // (drain_worker_snapshots), and everything else here is a
        // non-blocking poll.
        app.drain_boot_results();
        app.drain_worker_snapshots();
        app.drain_runs();
        app.poll_runs().await;
        app.drain_services();
        app.tick_watchers().await;

        terminal.draw(|f| ui::render(app, f))?;
        if app.should_quit() {
            return Ok(DriveOutcome::Quit);
        }
        if let Some(path) = app.take_pending_switch() {
            return Ok(DriveOutcome::SwitchWorktree {
                path,
                view: app.view(),
            });
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
            // Drain phantom events for a short window. Terminals
            // respond to tmux's mode-restore queries (DA, color
            // queries, etc.) with bytes on stdin that crossterm
            // parses as Events — verified in the wild: ghostty
            // emitted 76 keypresses including `d`s, which would
            // happily trigger `terminals_kill_selected` on the
            // brand-new shell window. Real user keypresses don't
            // arrive within 150ms of pressing ctrl+b d (their
            // fingers are still recovering from the chord); the
            // terminal's response bytes do.
            //
            // Set `SCAFFL_DEBUG_INPUT=1` to log every drained
            // event — useful when porting to a new terminal that
            // misbehaves in some other way.
            let verbose = std::env::var("SCAFFL_DEBUG_INPUT")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            let drain_deadline = std::time::Instant::now() + Duration::from_millis(150);
            let mut drained_count = 0usize;
            while std::time::Instant::now() < drain_deadline {
                match tokio::time::timeout(Duration::from_millis(30), events.recv()).await {
                    Ok(Some(ev)) => {
                        drained_count += 1;
                        if verbose {
                            app.diagnostic(format!("[input]   {ev:?}"));
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {} // timeout — keep waiting until the deadline
                }
            }
            if verbose && drained_count > 0 {
                app.diagnostic(format!(
                    "[input] discarded {drained_count} post-detach phantom event(s) — terminal mode-restore artefacts"
                ));
            }
            // Reload cached window list — new shell created, or
            // user typed `exit` and the window died.
            refresh_tmux_windows(app, true).await;
            // Paint the post-attach state *now* so the user sees
            // the refreshed terminals view immediately, without
            // waiting for the next event-driven render.
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
        if app.take_pending_lazygit() {
            // Mirror the tmux-attach handoff: drop the events
            // reader so its blocking poll thread doesn't fight
            // lazygit for stdin, leave alternate screen / cooked
            // mode, run lazygit (it inherits stdin/stdout/stderr),
            // then re-enter the TUI when the user q's out.
            drop(events);
            leave_terminal(terminal)?;
            if let Err(msg) = crate::lazygit::run(app.project_root()).await {
                app.flash = Some(msg.clone());
                app.diagnostic(format!("[lazygit] {msg}"));
            }
            *terminal = enter_terminal(&terminal_title(app))?;
            terminal.clear()?;
            events = spawn_event_reader();
            // Lazygit may have committed / staged / reset; the
            // current diff snapshot is no longer authoritative.
            app.diff_mark_stale();
            ensure_diff_loaded(app).await;
            terminal.draw(|f| ui::render(app, f))?;
            continue;
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
        [
            "set-option",
            "-t",
            &req.session,
            "destroy-unattached",
            "off",
        ],
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

/// Query tmux for the current window list of `session`. Format
/// uses `\t` between fields so window names / paths with spaces
/// or colons round-trip cleanly.
async fn list_tmux_windows(session: &str) -> WindowList {
    use tokio::process::Command;
    let output = match Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index}\t#{window_name}\t#{pane_current_path}",
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

/// Capture the visible content of a tmux pane as a `Vec<String>`,
/// one entry per visible row. Errors / missing panes return an
/// empty Vec so the renderer can fall back to its hint text.
async fn capture_pane(session: &str, window: u32) -> Vec<String> {
    use tokio::process::Command;
    let target = format!("{session}:{window}");
    // `-e` keeps SGR escape sequences in the dump so the renderer
    // can reapply prompt / ls / cargo coloring; `-p` writes to stdout
    // instead of a paste buffer.
    let Ok(output) = Command::new("tmux")
        .args(["capture-pane", "-e", "-p", "-t", &target])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

/// Parse the tab-separated `tmux list-windows -F` output we ask
/// for. Lines look like `0\tzsh\t/home/me/proj` or
/// `1\tsvc:app\t/`. The path may be empty for windows that haven't
/// launched a process yet — preserved as `None`. Public for tests.
pub(crate) fn parse_tmux_windows(input: &str) -> Vec<crate::app::TmuxWindow> {
    let mut out = Vec::new();
    for line in input.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(idx_str) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let Ok(index) = idx_str.parse::<u32>() else {
            continue;
        };
        let cwd = parts.next().map(str::to_string).filter(|s| !s.is_empty());
        out.push(crate::app::TmuxWindow {
            index,
            name: name.to_string(),
            cwd,
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
            // Capture each window's visible pane content for the
            // info-pane preview. ~5-15ms per window via
            // `tmux capture-pane -p`; cheap enough to refresh on
            // every windows reload.
            let indices: Vec<u32> = w.iter().map(|win| win.index).collect();
            for idx in &indices {
                let lines = capture_pane(&session, *idx).await;
                app.terminals_set_preview(*idx, lines);
            }
            app.terminals_set_windows(w);
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
        // Worktree switcher is also global — accessible from every
        // view (used to be control-center-only).
        KeyCode::Char('W') => {
            let entries = build_worktree_rows(app).await;
            app.open_worktree_switcher(entries);
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
        // `W` is handled in the global view-switch block above so
        // it works from every view, not just the control center.
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
        // `n` is the shortcut for the `+ new shell` sentinel —
        // hand-shortcut for power users who don't want to scroll
        // to the bottom of the list every time.
        KeyCode::Char('n') => app.queue_new_shell(),
        KeyCode::Enter => app.terminals_confirm(),
        _ => {}
    }
}

async fn handle_key_diff(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    use crate::app::DiffFocus;
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
            app.diff_mark_stale();
            ensure_diff_loaded(app).await;
            return;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.diff_toggle_focus();
            return;
        }
        KeyCode::Char('w') => {
            app.diff_toggle_wrap();
            return;
        }
        KeyCode::Char('L') => {
            if app.diff().lazygit_available {
                app.request_lazygit();
            } else {
                app.flash = Some("install lazygit to enable the L keybind".into());
            }
            return;
        }
        KeyCode::Char(']') => {
            app.diff_set_focus(DiffFocus::Body);
            app.diff_jump_hunk_next();
            return;
        }
        KeyCode::Char('[') => {
            app.diff_set_focus(DiffFocus::Body);
            app.diff_jump_hunk_prev();
            return;
        }
        _ => {}
    }
    match app.diff_focus() {
        DiffFocus::Files => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                app.diff_select_next();
                ensure_diff_for_selected(app).await;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.diff_select_prev();
                ensure_diff_for_selected(app).await;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                app.diff_set_focus(DiffFocus::Body);
            }
            _ => {}
        },
        DiffFocus::Body => match code {
            KeyCode::Down | KeyCode::Char('j') => app.diff_body_scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => app.diff_body_scroll_by(-1),
            KeyCode::PageDown => {
                let half = (app.diff().body_height.get() / 2).max(1) as i32;
                app.diff_body_scroll_by(half);
            }
            KeyCode::PageUp => {
                let half = (app.diff().body_height.get() / 2).max(1) as i32;
                app.diff_body_scroll_by(-half);
            }
            KeyCode::Home => app.diff_body_scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => app.diff_body_scroll_to_bottom(),
            KeyCode::Char('g') if app.diff_consume_g_chord() => {
                app.diff_body_scroll_to_top();
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                app.diff_set_focus(DiffFocus::Files);
            }
            _ => {}
        },
    }
}

/// Resolve the trunk branch (config override → remote default →
/// local fallback), its merge-base with HEAD, the current branch
/// name, and the 7-char short anchor SHA — then store all four on
/// the app. Cheap enough to redo on every diff refresh — the trunk
/// can move forward (`git pull origin main`) and we want subsequent
/// diffs to anchor against the new merge-base.
async fn refresh_diff_anchor(app: &mut App) {
    let project_root = app.project_root().to_path_buf();
    let configured = app.config().diff.base.clone();
    let trunk = scaffl_runtime::detect_trunk(&project_root, configured.as_deref()).await;
    let anchor = match trunk.as_deref() {
        Some(t) => scaffl_runtime::merge_base(&project_root, t).await,
        None => None,
    };
    let branch = current_branch(&project_root).await;
    let anchor_short = anchor.as_deref().map(|sha| {
        sha.chars().take(7).collect::<String>()
    });
    app.diff_set_anchor(trunk, anchor, branch, anchor_short);
}

/// Resolve the current branch name (`git rev-parse --abbrev-ref HEAD`).
/// Returns None when detached or the command fails — the banner just
/// hides the branch slot in that case.
pub(crate) async fn current_branch(project_root: &std::path::Path) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" { None } else { Some(s) }
}

/// Populate the diff file list if it hasn't been loaded yet, and
/// ensure the selected file's diff body is cached. Cheap on
/// subsequent calls thanks to the per-file cache.
async fn ensure_diff_loaded(app: &mut App) {
    // If the boot preload is still in flight, prefer awaiting it over
    // firing duplicate git commands. On a manual refresh (`r` / post-
    // lazygit) the boot rx is already drained, so this returns
    // immediately.
    app.await_diff_preload().await;
    if !app.diff().loaded {
        // Refresh anchor on every reload so a freshly-pulled trunk
        // shifts the comparison forward instead of staying pinned to
        // the merge-base we resolved at startup.
        refresh_diff_anchor(app).await;
        let project_root = app.project_root().to_path_buf();
        let anchor = app.diff().anchor.clone();
        match load_diff_files(&project_root, anchor.as_deref()).await {
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
    let anchor = app.diff().anchor.clone();
    let lines = load_diff_for_file(&project_root, &file, anchor.as_deref()).await;
    app.diff_set_cache(file.path.clone(), lines);
}

/// Build the changed-file list. With `anchor` set, we want
/// "everything that differs from the merge-base, plus untracked
/// files" — that's `git diff --name-status <anchor>` (committed
/// since branching + working-tree changes against tracked files)
/// merged with `git ls-files --others --exclude-standard`
/// (currently-untracked files). Without an anchor, fall back to
/// `git status --porcelain` so we still work in repos where no
/// trunk could be detected (e.g. fresh `git init` with no commits
/// past HEAD).
pub(crate) async fn load_diff_files(
    project_root: &std::path::Path,
    anchor: Option<&str>,
) -> Result<Vec<crate::app::DiffFile>, String> {
    use std::collections::BTreeMap;
    let Some(anchor) = anchor else {
        return load_diff_files_fallback(project_root).await;
    };

    // Three queries in parallel: name+status, churn (numstat), and
    // untracked. Saves ~100ms on cold cache vs the previous
    // sequential path.
    let diff_fut = tokio::process::Command::new("git")
        .args(["diff", "--name-status", anchor])
        .current_dir(project_root)
        .output();
    let numstat_fut = tokio::process::Command::new("git")
        .args(["diff", "--numstat", anchor])
        .current_dir(project_root)
        .output();
    let untracked_fut = tokio::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(project_root)
        .output();
    let (diff_out, numstat_out, untracked_out) =
        tokio::join!(diff_fut, numstat_fut, untracked_fut);
    let diff_out = diff_out.map_err(|e| format!("git diff --name-status failed: {e}"))?;
    if !diff_out.status.success() {
        // Anchor invalid (rare — `merge_base` already returned Some)
        // — fall back to porcelain so the view still works.
        return load_diff_files_fallback(project_root).await;
    }
    let numstat_out = numstat_out.map_err(|e| format!("git diff --numstat failed: {e}"))?;
    let untracked_out = untracked_out.map_err(|e| format!("git ls-files failed: {e}"))?;

    // Merge into a BTreeMap keyed by path so a file that's both
    // tracked-modified AND showing up in ls-files (shouldn't happen,
    // but defensive) doesn't appear twice.
    use crate::app::{DiffFile, DiffStatus};
    let mut files: BTreeMap<String, DiffFile> = BTreeMap::new();
    let diff_text = String::from_utf8_lossy(&diff_out.stdout);
    for entry in parse_diff_name_status(&diff_text) {
        files.insert(
            entry.path.clone(),
            DiffFile {
                path: entry.path,
                status: entry.status,
                additions: 0,
                deletions: 0,
                binary: false,
            },
        );
    }
    if numstat_out.status.success() {
        let numstat_text = String::from_utf8_lossy(&numstat_out.stdout);
        for entry in parse_numstat(&numstat_text) {
            if let Some(f) = files.get_mut(&entry.path) {
                f.additions = entry.additions;
                f.deletions = entry.deletions;
                f.binary = entry.binary;
            }
        }
    }
    if untracked_out.status.success() {
        let untracked_text = String::from_utf8_lossy(&untracked_out.stdout);
        for path in untracked_text.lines() {
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            // Untracked churn (lines added) is computed lazily on
            // first body load — leave 0/0 here. The list still shows
            // the U status badge so users can tell.
            files.entry(path.to_string()).or_insert_with(|| DiffFile {
                path: path.to_string(),
                status: DiffStatus::Untracked,
                additions: 0,
                deletions: 0,
                binary: false,
            });
        }
    }
    Ok(files.into_values().collect())
}

pub(crate) struct NumstatEntry {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

/// Parse `git diff --numstat <anchor>` output. Each line is
/// `<add>\t<del>\t<path>`. Binary files report `-\t-\t<path>`. We
/// keep the path through rename arrows verbatim — the BTreeMap
/// merge in the caller is keyed by path, so a rename whose
/// destination already appears in `--name-status` will get its
/// churn merged correctly.
pub(crate) fn parse_numstat(input: &str) -> Vec<NumstatEntry> {
    let mut out = Vec::new();
    for line in input.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(add) = parts.next() else { continue };
        let Some(del) = parts.next() else { continue };
        let Some(path) = parts.next() else { continue };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        // Renames: `git diff --numstat` emits `path => newpath` or
        // `{old => new}/file` style. Take the destination so it
        // matches the path in `--name-status`.
        let dest = if let Some(idx) = path.find(" => ") {
            path[idx + 4..]
                .trim_end_matches('}')
                .trim_start_matches('{')
                .to_string()
        } else {
            path.to_string()
        };
        let binary = add == "-" && del == "-";
        let additions = if binary { 0 } else { add.parse().unwrap_or(0) };
        let deletions = if binary { 0 } else { del.parse().unwrap_or(0) };
        out.push(NumstatEntry {
            path: dest,
            additions,
            deletions,
            binary,
        });
    }
    out
}

/// Old behaviour, kept as a fallback when no trunk could be
/// detected: list whatever the working tree differs from HEAD on.
async fn load_diff_files_fallback(
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

/// Parse `git diff --name-status <anchor>` output. Each line is one
/// status letter + tab + path (rename = `R<similarity>\told\tnew`).
/// Untracked files don't appear here — the caller pulls them
/// separately from `git ls-files --others`.
pub(crate) fn parse_diff_name_status(input: &str) -> Vec<DiffNameStatusEntry> {
    use crate::app::DiffStatus;
    let mut out = Vec::new();
    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let Some(status_field) = parts.next() else {
            continue;
        };
        let letter = status_field.chars().next().unwrap_or(' ');
        let status = match letter {
            'A' => DiffStatus::Added,
            'D' => DiffStatus::Deleted,
            'M' => DiffStatus::Modified,
            'R' => DiffStatus::Renamed,
            'C' => DiffStatus::Other, // copy
            _ => DiffStatus::Other,
        };
        // Rename rows have two paths; we want the *destination*.
        // Empty path field → malformed; skip rather than emit a row
        // with an empty path that would render as a blank sidebar
        // entry.
        let path = match (parts.next(), parts.next()) {
            (Some(_old), Some(new)) if !new.is_empty() => new.to_string(),
            (Some(p), None) if !p.is_empty() => p.to_string(),
            _ => continue,
        };
        out.push(DiffNameStatusEntry { path, status });
    }
    out
}

pub(crate) struct DiffNameStatusEntry {
    pub path: String,
    pub status: crate::app::DiffStatus,
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
        out.push(DiffFile {
            path,
            status,
            additions: 0,
            deletions: 0,
            binary: false,
        });
    }
    out
}

async fn load_diff_for_file(
    project_root: &std::path::Path,
    file: &crate::app::DiffFile,
    anchor: Option<&str>,
) -> Vec<crate::app::DiffLine> {
    use crate::app::{DiffLine, DiffLineKind, DiffStatus};
    // Untracked files don't exist in HEAD or the anchor — git diff
    // would error. Synthesise a file-as-added view with the file
    // contents prefixed by `+`.
    if file.status == DiffStatus::Untracked {
        return load_untracked_as_diff(project_root, &file.path).await;
    }
    let base = anchor.unwrap_or("HEAD");
    let output = match tokio::process::Command::new("git")
        .args(["diff", base, "--", &file.path])
        .current_dir(project_root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return vec![DiffLine {
                kind: DiffLineKind::Header,
                text: format!("git diff failed: {e}"),
                old_lineno: None,
                new_lineno: None,
                spans: vec![],
            }];
        }
    };
    let body = String::from_utf8_lossy(&output.stdout);
    enrich_diff_lines(&body, &file.path)
}

/// Walk a unified-diff body and produce `DiffLine`s with per-line
/// line-numbers and syntect spans pre-computed.
///
/// Hunk headers (`@@ -A,B +C,D @@`) reset the `(old, new)` counters
/// so the gutter renders the same line numbers `git diff` would
/// print. Each non-hunk, non-header line goes through syntect once,
/// using the file's path to pick a syntax — avoids redoing the
/// lookup on every frame as the user scrolls.
pub(crate) fn enrich_diff_lines(body: &str, path: &str) -> Vec<crate::app::DiffLine> {
    use crate::app::{DiffLine, DiffLineKind};
    let mut out = Vec::new();
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;
    for raw in body.lines() {
        let kind = DiffLineKind::classify(raw);
        match kind {
            DiffLineKind::Hunk => {
                if let Some((o, n)) = parse_hunk_header(raw) {
                    old_no = o;
                    new_no = n;
                }
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                });
            }
            DiffLineKind::Header => {
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                });
            }
            DiffLineKind::Added => {
                let inner = raw.get(1..).unwrap_or("");
                let spans = crate::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: Some(new_no),
                    spans,
                });
                new_no = new_no.saturating_add(1);
            }
            DiffLineKind::Removed => {
                let inner = raw.get(1..).unwrap_or("");
                let spans = crate::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: Some(old_no),
                    new_lineno: None,
                    spans,
                });
                old_no = old_no.saturating_add(1);
            }
            DiffLineKind::Context => {
                let inner = raw.strip_prefix(' ').unwrap_or(raw);
                let spans = crate::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: Some(old_no),
                    new_lineno: Some(new_no),
                    spans,
                });
                old_no = old_no.saturating_add(1);
                new_no = new_no.saturating_add(1);
            }
        }
    }
    out
}

/// Parse the leading `(old_start, new_start)` out of a hunk header
/// like `@@ -10,7 +10,9 @@`. Returns None on malformed input —
/// callers leave the counters where they were, which is harmless.
pub(crate) fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // Skip the leading `@@`.
    let after = line.strip_prefix("@@")?;
    let mut tokens = after.split_whitespace();
    let old = tokens.next()?.strip_prefix('-')?;
    let new = tokens.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
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
        old_lineno: None,
        new_lineno: None,
        spans: vec![],
    }];
    let mut new_no: u32 = 1;
    for l in body.lines() {
        let spans = crate::syntax::highlight_inner(path, l);
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("+{l}"),
            old_lineno: None,
            new_lineno: Some(new_no),
            spans,
        });
        new_no = new_no.saturating_add(1);
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
        KeyCode::Enter => match app.switcher_confirm() {
            crate::app::SwitcherConfirm::OpenCreateForm => {
                let project_root = app.project_root().to_path_buf();
                let branches = scaffl_runtime::list_branches(&project_root).await;
                // Anchor new worktrees against the git toplevel's
                // parent so they land next to the repo no matter
                // where scaffl was invoked from (e.g. running in
                // `<repo>/tmp/test` shouldn't push them into tmp/).
                let parent = scaffl_runtime::git_toplevel(&project_root)
                    .await
                    .and_then(|tl| tl.parent().map(|p| p.to_path_buf()));
                app.open_create_form(branches, parent);
            }
            crate::app::SwitcherConfirm::Switched | crate::app::SwitcherConfirm::NoOp => {}
        },
        _ => {}
    }
}

async fn handle_key_switcher_form(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.switcher_form_cancel(),
        KeyCode::Tab => app.switcher_form_toggle_focus(),
        KeyCode::Up => app.switcher_form_select_prev(),
        KeyCode::Down => app.switcher_form_select_next(),
        KeyCode::Backspace => app.switcher_form_pop_char(),
        KeyCode::Enter => {
            // Resolve the form into (path, BranchSpec); shell out
            // to git; report back via switcher_form_finish.
            let Some(action) = app.switcher_form_resolve() else {
                return;
            };
            let project_root = app.project_root().to_path_buf();
            let result = create_worktree(&project_root, &action).await;
            app.switcher_form_finish(result);
        }
        KeyCode::Char(c) => app.switcher_form_push_char(c),
        _ => {}
    }
}

/// Run `git worktree add` for the user's resolved form action.
/// `BranchSpec::Existing(name)` → `git worktree add <path> <name>`
/// (git auto-creates a tracking branch when `name` matches a
/// remote-only ref). `BranchSpec::CreateOff(name)` →
/// `git worktree add <path> -b <name>` (new branch off HEAD).
///
/// Returns the canonicalised path on success so the App rebuild
/// has a stable absolute target; trimmed git stderr on failure so
/// the modal renders the diagnostic.
async fn create_worktree(
    project_root: &std::path::Path,
    action: &crate::app::NewWorktreeAction,
) -> Result<std::path::PathBuf, String> {
    use crate::app::BranchSpec;
    let path = action.path.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    let mut argv: Vec<String> = vec!["worktree".into(), "add".into()];
    match &action.branch {
        BranchSpec::Existing(name) => {
            argv.push(path.to_string());
            argv.push(name.clone());
        }
        BranchSpec::CreateOff(name) => {
            argv.push("-b".into());
            argv.push(name.clone());
            argv.push(path.to_string());
        }
    }
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
    async fn capital_w_works_from_terminals_view() {
        // Used to be control-center-only; now global.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.switch_view(crate::app::View::Terminals);
        handle_event(&mut app, press(KeyCode::Char('W'))).await;
        assert_eq!(app.mode(), crate::app::Mode::WorktreeSwitcher);
    }

    #[tokio::test]
    async fn capital_w_works_from_diff_view() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.switch_view(crate::app::View::Diff);
        handle_event(&mut app, press(KeyCode::Char('W'))).await;
        assert_eq!(app.mode(), crate::app::Mode::WorktreeSwitcher);
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
        // \t-separated: index, window_name, pane_current_path.
        let input = "0\tzsh\t/home/me/proj\n1\tsvc:app\t/\n2\tvim\t/home/me/proj/src\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].index, 0);
        assert_eq!(windows[0].name, "zsh");
        assert_eq!(windows[0].cwd.as_deref(), Some("/home/me/proj"));
        assert_eq!(windows[1].name, "svc:app");
        assert_eq!(windows[1].cwd.as_deref(), Some("/"));
        assert_eq!(windows[2].index, 2);
        assert_eq!(windows[2].cwd.as_deref(), Some("/home/me/proj/src"));
    }

    #[test]
    fn parse_tmux_windows_handles_missing_cwd() {
        // pane_current_path can be empty for fresh windows.
        let input = "0\tzsh\t\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].name, "zsh");
        assert!(windows[0].cwd.is_none());
    }

    #[test]
    fn parse_tmux_windows_skips_malformed_lines() {
        let input = "0\tzsh\t/home/me\nnot a window\n2\tvim\t/home/me/src\n";
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

    #[test]
    fn parse_diff_name_status_basic() {
        use crate::app::DiffStatus;
        let input = "M\tsrc/main.rs\nA\tsrc/lib.rs\nD\tCargo.toml\nR090\told.txt\tnew.txt\n";
        let entries = parse_diff_name_status(input);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].status, DiffStatus::Modified);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[1].status, DiffStatus::Added);
        assert_eq!(entries[2].status, DiffStatus::Deleted);
        // Renames take the destination path, not the source.
        assert_eq!(entries[3].status, DiffStatus::Renamed);
        assert_eq!(entries[3].path, "new.txt");
    }

    #[test]
    fn parse_diff_name_status_skips_blank_and_malformed() {
        let input = "\nM\n\t\nM\tok.rs\n";
        let entries = parse_diff_name_status(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "ok.rs");
    }

    #[test]
    fn parse_numstat_text_and_binary() {
        let input = "10\t3\tsrc/main.rs\n0\t5\tCargo.toml\n-\t-\tassets/logo.png\n";
        let entries = parse_numstat(input);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].additions, 10);
        assert_eq!(entries[0].deletions, 3);
        assert!(!entries[0].binary);
        assert_eq!(entries[1].additions, 0);
        assert_eq!(entries[1].deletions, 5);
        assert_eq!(entries[2].path, "assets/logo.png");
        assert!(entries[2].binary);
        assert_eq!(entries[2].additions, 0);
        assert_eq!(entries[2].deletions, 0);
    }

    #[test]
    fn parse_numstat_rename_uses_destination_path() {
        // git diff --numstat emits renames as `path => newpath`.
        let input = "5\t2\told.txt => new.txt\n";
        let entries = parse_numstat(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "new.txt");
        assert_eq!(entries[0].additions, 5);
    }

    #[test]
    fn parse_hunk_header_basic() {
        // (old_start, new_start) — the counts after the comma are
        // ignored because we only need the starting offsets.
        assert_eq!(parse_hunk_header("@@ -1,7 +1,9 @@"), Some((1, 1)));
        assert_eq!(parse_hunk_header("@@ -100 +200 @@ fn foo()"), Some((100, 200)));
        assert_eq!(parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn enrich_diff_lines_tracks_line_numbers_through_hunks() {
        use crate::app::DiffLineKind;
        // Two hunks: an add+remove pair around line 1, then a context
        // run starting at line 10.
        let body = "\
diff --git a/foo.rs b/foo.rs
index abc..def 100644
--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,3 @@
-let a = 1;
+let a = 2;
 keep
@@ -10,2 +10,3 @@
 keep10
+inserted
";
        let lines = enrich_diff_lines(body, "foo.rs");
        // Filter to the kinds we care about for line-number tracking.
        let pick: Vec<(&str, Option<u32>, Option<u32>)> = lines
            .iter()
            .filter(|l| matches!(
                l.kind,
                DiffLineKind::Added | DiffLineKind::Removed | DiffLineKind::Context,
            ))
            .map(|l| (l.text.as_str(), l.old_lineno, l.new_lineno))
            .collect();
        assert_eq!(
            pick,
            vec![
                ("-let a = 1;", Some(1), None),
                ("+let a = 2;", None, Some(1)),
                (" keep", Some(2), Some(2)),
                (" keep10", Some(10), Some(10)),
                ("+inserted", None, Some(11)),
            ]
        );
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
