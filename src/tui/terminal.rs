//! Terminal lifecycle and event loop.
//!
//! Owns the raw-mode / alternate-screen handshake and pipes crossterm events
//! through an mpsc channel. The render and event dispatch logic lives here;
//! application semantics live in [`crate::tui::app`].

use crate::tui::TuiError;
use crate::tui::app::{
    App, AttachRequest, ClickTarget, Command, DOUBLE_CLICK_WINDOW, KillWindow, LaunchRejection,
    Mode, View,
};
use crate::tui::dialogs::switcher::{BranchSpec, NewWorktreeAction, SwitcherConfirm, WorktreeRow};
use crate::tui::ui;
use crate::tui::views::diff::git::ensure_diff_loaded;
use crate::tui::views::terminals::tmux::{attach_tmux, refresh_tmux_windows};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{Stdout, Write, stdout};
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
        view: View,
    },
}

pub async fn run_event_loop(
    app: &mut App,
    initial_view: View,
) -> Result<DriveOutcome, TuiError> {
    let title = terminal_title(app);
    let mut terminal = enter_terminal(&title)?;
    // Run the same view-entry hooks the keymap fires when switching
    // views interactively — so a worktree hot-reload that lands in
    // Terminals or Diff doesn't show stale (empty) state on first
    // paint.
    match initial_view {
        View::Terminals => {
            app.request_tmux_probe();
            refresh_tmux_windows(app, false).await;
        }
        View::Diff => {
            ensure_diff_loaded(app).await;
        }
        View::ControlCenter => {}
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
        .unwrap_or_else(|| "keel".into());
    format!("keel — {project}")
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
    'outer: loop {
        // Pre-render hooks: drain queued output and advance run state.
        // All blocking I/O lives off this path — boot tasks deliver
        // their results via channels (drain_boot_results), the
        // worker polls service status on its own cadence
        // (drain_worker_snapshots), and everything else here is a
        // non-blocking poll.
        app.drain_boot_results();
        app.drain_worker_snapshots();
        app.drain_messages();
        app.drain_runs();
        app.poll_runs().await;
        app.drain_services();
        app.tick_watchers().await;

        terminal.draw(|f| ui::render(app, f))?;
        if app.should_quit() {
            return Ok(DriveOutcome::Quit);
        }
        // Drain queued commands from handlers. One match arm per
        // variant — adding a new async side effect is one variant in
        // `Command` plus one arm here, no new `take_pending_*`
        // accessor or `if let Some(_) = ...` drain block to wire up.
        for cmd in app.drain_commands() {
            match cmd {
                Command::EmitBell => {
                    // Edge-triggered: armed only when a window's
                    // `has_bell` flipped false→true on the most recent
                    // worker snapshot. Written after the draw because
                    // ratatui has just released the terminal; `\x07`
                    // doesn't move the cursor, so it's safe between
                    // frames.
                    let mut out = stdout();
                    let _ = out.write_all(b"\x07");
                    let _ = out.flush();
                }
                Command::SwitchWorktree(path) => {
                    return Ok(DriveOutcome::SwitchWorktree {
                        path,
                        view: app.view(),
                    });
                }
                Command::AttachTmux(req) => {
                    run_attach(app, req, &mut events, terminal).await?;
                    continue 'outer;
                }
                Command::KillTmuxWindow(kill) => {
                    run_kill_window(app, kill).await;
                }
                Command::OpenLazygit => {
                    run_lazygit(app, &mut events, terminal).await?;
                    continue 'outer;
                }
            }
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

/// Handle one queued [`Command::AttachTmux`]: ensure-up the
/// devcontainer if required (else flash + bail), leave the alternate
/// screen, run `tmux attach`, drain phantom mode-restore events,
/// silence the next bell baseline, refresh the window list, and
/// repaint. Caller should `continue 'outer` after this returns so the
/// regular event-loop branches restart.
async fn run_attach(
    app: &mut App,
    req: AttachRequest,
    events: &mut mpsc::UnboundedReceiver<Event>,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), TuiError> {
    // If the attach needs a devcontainer ensure_up, run it
    // *before* yielding to tmux. A failed ensure_up flashes
    // an error and drops the attach — better than handing
    // the user a tmux window that dies the second
    // `docker exec` realises the container isn't there.
    if let Some(ensure) = &req.ensure
        && let Err(e) = ensure.backend.ensure_up().await
    {
        app.flash(format!("devcontainer ensure-up failed: {e}"));
        return Ok(());
    }
    // Yield the terminal to tmux. Drop the events reader
    // first so its blocking poll thread doesn't fight tmux
    // for input, leave alternate screen / cooked mode, run
    // tmux attach (it inherits stdin/stdout/stderr from us),
    // then re-enter the TUI when the user detaches.
    let old_events = std::mem::replace(events, mpsc::unbounded_channel().1);
    drop(old_events);
    leave_terminal(terminal)?;
    attach_tmux(&req).await;
    *terminal = enter_terminal(&terminal_title(app))?;
    terminal.clear()?;
    *events = spawn_event_reader();
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
    // Set `KEEL_DEBUG_INPUT=1` to log every drained
    // event — useful when porting to a new terminal that
    // misbehaves in some other way.
    let verbose = std::env::var("KEEL_DEBUG_INPUT")
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
    // Bells that rang during the attach already played
    // through tmux's `bell-action any` to the outer
    // terminal — silence the next refresh so coming back to
    // keel doesn't double-fire the BEL for windows whose
    // flag is still set. Also discard any tmux snapshots the
    // worker queued mid-attach: applying them after the
    // synchronous refresh below would risk a stale
    // false→true transition for a flag that's since cleared.
    app.discard_pending_tmux_snapshots();
    app.terminals_mut().silence_next_bell();
    // Reload cached window list — new shell created, or
    // user typed `exit` and the window died.
    refresh_tmux_windows(app, true).await;
    // Paint the post-attach state *now* so the user sees
    // the refreshed terminals view immediately, without
    // waiting for the next event-driven render.
    terminal.draw(|f| ui::render(app, f))?;
    Ok(())
}

/// Handle one queued [`Command::KillTmuxWindow`]: shell out to
/// `tmux kill-window` and refresh the windows list. No flash on a
/// session that's now gone — that's the normal "killed the last
/// window" path.
async fn run_kill_window(app: &mut App, kill: KillWindow) {
    let _ = tokio::process::Command::new("tmux")
        .args([
            "kill-window",
            "-t",
            &format!("{}:{}", kill.session, kill.index),
        ])
        .status()
        .await;
    refresh_tmux_windows(app, false).await;
}

/// Handle one queued [`Command::OpenLazygit`]: mirror the tmux-attach
/// handoff (drop the events reader, leave alt screen, run lazygit
/// inheriting stdio, re-enter), then mark the diff stale and reload
/// so a commit / stage / reset inside lazygit shows up immediately.
/// Caller should `continue 'outer` after this returns.
async fn run_lazygit(
    app: &mut App,
    events: &mut mpsc::UnboundedReceiver<Event>,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), TuiError> {
    let old_events = std::mem::replace(events, mpsc::unbounded_channel().1);
    drop(old_events);
    leave_terminal(terminal)?;
    if let Err(msg) = crate::tui::lazygit::run(app.project_root()).await {
        app.flash(msg.clone());
        app.diagnostic(format!("[lazygit] {msg}"));
    }
    *terminal = enter_terminal(&terminal_title(app))?;
    terminal.clear()?;
    *events = spawn_event_reader();
    // Lazygit may have committed / staged / reset; the
    // current diff snapshot is no longer authoritative.
    app.diff_mut().mark_stale();
    ensure_diff_loaded(app).await;
    terminal.draw(|f| ui::render(app, f))?;
    Ok(())
}

pub(crate) async fn handle_event(app: &mut App, event: Event) {
    match event {
        Event::Key(KeyEvent {
            kind: KeyEventKind::Press,
            code,
            modifiers,
            ..
        }) => {
            // Any key dismisses a stale flash. The dispatched handler
            // may re-arm a fresh one for this event.
            app.clear_flash();
            match app.mode() {
                Mode::Normal => handle_key_normal(app, code, modifiers).await,
                Mode::Palette => handle_key_palette(app, code, modifiers).await,
                Mode::Confirm => handle_key_confirm(app, code, modifiers),
                Mode::ArgsPrompt => handle_key_args_prompt(app, code, modifiers),
                Mode::WorktreeSwitcher => {
                    handle_key_switcher(app, code, modifiers).await
                }
            }
        }
        Event::Mouse(me) => handle_mouse(app, me).await,
        Event::Resize(_, _) => {
            // The next draw call already adapts to the new size.
        }
        _ => {}
    }
}

/// Mouse router. Branches **mode first, view second** — same priority
/// the keyboard dispatch uses — so an open overlay (palette, confirm,
/// switcher) always takes precedence over the underlying view's
/// click handler. Stray clicks (e.g. inside the args-prompt's text
/// input or on the brief gap between rendered rows) fall through to
/// no-op.
async fn handle_mouse(app: &mut App, me: MouseEvent) {
    match app.mode() {
        Mode::Palette => handle_mouse_palette(app, me).await,
        Mode::Confirm => handle_mouse_confirm(app, me),
        Mode::WorktreeSwitcher => handle_mouse_switcher(app, me).await,
        // Text-input modes ignore mouse: there's no row to click and
        // we don't (yet) reposition the cursor on click.
        Mode::ArgsPrompt => {}
        Mode::Normal => match app.view() {
            View::ControlCenter => handle_mouse_control_center(app, me).await,
            View::Terminals => crate::tui::views::terminals::input::handle_mouse(app, me),
            View::Diff => crate::tui::views::diff::input::handle_mouse(app, me).await,
        },
    }
}

/// Hit-test `(col, row)` against a list of per-row rects and return
/// the matching index. Linear scan — these vecs are tiny (a few
/// dozen rows at most) so a binary search by y wouldn't pay back the
/// added complexity.
pub(crate) fn hit_test(rects: &[ratatui::layout::Rect], col: u16, row: u16) -> Option<usize> {
    rects.iter().position(|r| rect_contains(*r, col, row))
}

/// Common click resolution: emit `Select` for a first click on
/// `target`, or `Activate` when the same target was clicked again
/// within [`DOUBLE_CLICK_WINDOW`]. Updates
/// `app.last_click` as a side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickKind {
    Select,
    Activate,
}

pub(crate) fn resolve_click(app: &mut App, target: ClickTarget) -> ClickKind {
    let now = std::time::Instant::now();
    let activate = app
        .last_click
        .map(|(t, prev)| {
            prev == target && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
        })
        .unwrap_or(false);
    if activate {
        // Reset so a triple-click doesn't re-activate.
        app.last_click = None;
        ClickKind::Activate
    } else {
        app.last_click = Some((now, target));
        ClickKind::Select
    }
}

pub(crate) fn rect_contains(rect: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Three lines per wheel notch matches the j/k cadence closely
/// enough that mixing keyboard and wheel doesn't feel jumpy.
pub(crate) const WHEEL_LINES: i32 = 3;

/// Mouse handler for the control-center view: click a sidebar row
/// to select it, double-click within
/// [`DOUBLE_CLICK_WINDOW`] to run the same
/// activation as Enter. Scroll-wheel moves the selection up/down at
/// the keyboard cadence.
async fn handle_mouse_control_center(app: &mut App, me: MouseEvent) {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Hit-test inside a tight scope so the RefCell borrow is
            // gone before any `.await` — clippy's
            // `await_holding_refcell_ref` lint would otherwise fire.
            let hit = {
                let rects = app.sidebar_item_rects.borrow();
                hit_test(&rects, me.column, me.row)
            };
            let Some(idx) = hit else {
                return;
            };
            let target = ClickTarget::SidebarItem(idx);
            match resolve_click(app, target) {
                ClickKind::Select => app.select_at(idx),
                ClickKind::Activate => {
                    // Make sure the activation runs against the row
                    // we just clicked, even if the previous selection
                    // pointed elsewhere when the timer started.
                    app.select_at(idx);
                    activate_control_center_selection(app).await;
                }
            }
        }
        MouseEventKind::ScrollDown => {
            for _ in 0..WHEEL_LINES {
                app.select_next();
            }
        }
        MouseEventKind::ScrollUp => {
            for _ in 0..WHEEL_LINES {
                app.select_prev();
            }
        }
        _ => {}
    }
}

/// Mouse handler for the command palette overlay. Click → select;
/// double-click → run the selected entry (same path the Enter key
/// takes in `handle_key_palette`).
async fn handle_mouse_palette(app: &mut App, me: MouseEvent) {
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
                    activate_palette_selection(app);
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

/// Run the palette's selected entry. Mirrors the `KeyCode::Enter` arm
/// of `handle_key_palette`.
fn activate_palette_selection(app: &mut App) {
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

/// Resolve the switcher's selected row. Mirrors the `KeyCode::Enter`
/// arm of `handle_key_switcher` — including the async branch list /
/// toplevel fetch for the "+ new worktree" sentinel.
async fn activate_switcher_selection(app: &mut App) {
    match app.switcher_confirm() {
        SwitcherConfirm::OpenCreateForm => {
            let project_root = app.project_root().to_path_buf();
            let branches = crate::runtime::list_branches(&project_root).await;
            let parent = crate::runtime::git_toplevel(&project_root)
                .await
                .and_then(|tl| tl.parent().map(|p| p.to_path_buf()));
            app.open_create_form(branches, parent);
        }
        SwitcherConfirm::Switched | SwitcherConfirm::NoOp => {}
    }
}

/// Mouse handler for the worktree switcher overlay. Click → select;
/// double-click → switch / open the new-worktree form (the sentinel
/// row at the end of the entries list).
async fn handle_mouse_switcher(app: &mut App, me: MouseEvent) {
    // The new-worktree form is text-only; once it's open, clicks
    // shouldn't reset the list selection underneath. Drop the event.
    if app.switcher().is_some_and(|s| s.creating.is_some()) {
        return;
    }
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(idx) = app
                .switcher()
                .and_then(|s| hit_test(&s.row_rects.borrow(), me.column, me.row))
            else {
                return;
            };
            let target = ClickTarget::SwitcherRow(idx);
            match resolve_click(app, target) {
                ClickKind::Select => app.switcher_select_at(idx),
                ClickKind::Activate => {
                    app.switcher_select_at(idx);
                    activate_switcher_selection(app).await;
                }
            }
        }
        MouseEventKind::ScrollDown => app.switcher_select_next(),
        MouseEventKind::ScrollUp => app.switcher_select_prev(),
        _ => {}
    }
}

/// Mouse handler for the confirm dialog. Single-click on Yes / No
/// presses the corresponding button; nothing else (no concept of
/// "selected" button — the keyboard moves focus, the mouse acts
/// directly).
fn handle_mouse_confirm(app: &mut App, me: MouseEvent) {
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
            app.switch_view(View::Terminals);
            app.request_tmux_probe();
            // Don't expect a session yet — the user may not have
            // attached to anything during this keel session.
            refresh_tmux_windows(app, false).await;
            return;
        }
        KeyCode::Char('G') => {
            app.switch_view(View::Diff);
            ensure_diff_loaded(app).await;
            return;
        }
        KeyCode::Char('C') if app.view() != View::ControlCenter => {
            app.switch_view(View::ControlCenter);
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
    if app.view() == View::Terminals {
        crate::tui::views::terminals::input::handle_key(app, code, modifiers).await;
        return;
    }
    if app.view() == View::Diff {
        crate::tui::views::diff::input::handle_key(app, code, modifiers).await;
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
                app.flash("aborted run");
            } else if app.abort_lifecycle_run() {
                app.flash("aborted lifecycle run");
            } else if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(crate::container::service_action::STOP, &[service.as_str()])
                    .await
            {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Char('S') => {
            if app.abort_lifecycle_run() {
                app.flash("aborted lifecycle run");
            } else if let Err(rej) = app
                .run_service_action(crate::container::service_action::STOP, &[])
                .await
            {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Char('r') => {
            if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(
                        crate::container::service_action::RESTART,
                        &[service.as_str()],
                    )
                    .await
            {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Char('R') => {
            if let Err(rej) = app
                .run_service_action(crate::container::service_action::RESTART, &[])
                .await
            {
                app.flash(launch_message(rej));
            }
        }
        // `u`: up the selected service. Pairs with `U` (up all) just
        // like `r`/`R` and `s`/`S`. Enter on a service used to do
        // this; it now attaches instead, so explicit `u` for "up
        // without attaching" is back.
        KeyCode::Char('u') => {
            if let Some(service) = app.selected_service().map(|s| s.name.clone())
                && let Err(rej) = app
                    .run_service_action(crate::container::service_action::UP, &[service.as_str()])
                    .await
            {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Char('U') => {
            if let Err(rej) = app
                .run_service_action(crate::container::service_action::UP, &[])
                .await
            {
                app.flash(launch_message(rej));
            }
        }
        // `W` is handled in the global view-switch block above so
        // it works from every view, not just the control center.
        // `D`: down all. No lowercase counterpart — compose's `down` is
        // intrinsically project-wide; the per-service equivalent is
        // `stop` (bound to `s`).
        KeyCode::Char('D') => {
            if let Err(rej) = app
                .run_service_action(crate::container::service_action::DOWN, &[])
                .await
            {
                app.flash(launch_message(rej));
            }
        }
        KeyCode::Enter => activate_control_center_selection(app).await,
        _ => {}
    }
}

/// Resolve "activate the current row" semantics for the control
/// center. Shared between the Enter handler and the mouse double-
/// click handler.
///
/// Routing:
///   container         → no-op (use U/D/R/S; flashed by
///                       try_launch_selected)
///   service           → attach into a tmux pane (jumps to the
///                       Terminals view; ctrl+b d returns). Non-
///                       container services (systemd / custom) flash
///                       a hint instead — no shell to attach to.
///   recipe / script   → either open args prompt (if forward_args
///                       and not already running) or launch
///   watcher           → no-op (watchers fire on file change)
async fn activate_control_center_selection(app: &mut App) {
    if let Some(service) = app.selected_service().map(|s| s.name.clone()) {
        app.request_tmux_probe();
        if app.terminals().tmux_available == Some(false) {
            app.flash("tmux not installed — install it to attach");
        } else if let Err(msg) = app.queue_service_attach(&service) {
            app.flash(msg);
        }
    } else if app.selected_accepts_args() && !selected_is_running(app) {
        // Discoverability path: a `forward_args = true` row gets
        // a prompt so users see they can pass args. Power users
        // bypass via the palette (`:cmd foo bar`).
        app.open_args_prompt();
    } else {
        match app.try_launch_selected() {
            Ok(()) => {}
            Err(LaunchRejection::AlreadyRunning) => {
                app.open_kill_restart_confirm();
            }
            Err(rej) => {
                app.flash(launch_message(rej));
            }
        }
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
                Some(Err(LaunchRejection::AlreadyRunning)) => {
                    app.open_kill_restart_confirm();
                }
                Some(Err(rej)) => {
                    app.flash(launch_message(rej));
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


/// Build worktree-switcher rows for the current project. The current
/// worktree (matched by canonicalised path) is flagged so the modal
/// can render it differently and pre-select it.
async fn build_worktree_rows(app: &App) -> Vec<WorktreeRow> {
    let project_root = app.project_root().to_path_buf();
    let entries = crate::runtime::worktree::list_worktrees(&project_root).await;
    let current = std::fs::canonicalize(&project_root).unwrap_or(project_root);
    entries
        .into_iter()
        .map(|e| {
            let path_buf = std::path::PathBuf::from(&e.path);
            let canonical = std::fs::canonicalize(&path_buf).unwrap_or_else(|_| path_buf.clone());
            let slug = derive_slug_from_entry(&e);
            WorktreeRow {
                path: path_buf,
                branch: e.branch.clone(),
                slug,
                is_current: canonical == current,
            }
        })
        .collect()
}

fn derive_slug_from_entry(e: &crate::runtime::WorktreeListEntry) -> String {
    if let Some(branch) = e.branch.as_deref() {
        return crate::runtime::worktree::slugify(branch);
    }
    if e.detached {
        return crate::runtime::worktree::slugify(
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
            SwitcherConfirm::OpenCreateForm => {
                let project_root = app.project_root().to_path_buf();
                let branches = crate::runtime::list_branches(&project_root).await;
                // Anchor new worktrees against the git toplevel's
                // parent so they land next to the repo no matter
                // where keel was invoked from (e.g. running in
                // `<repo>/tmp/test` shouldn't push them into tmp/).
                let parent = crate::runtime::git_toplevel(&project_root)
                    .await
                    .and_then(|tl| tl.parent().map(|p| p.to_path_buf()));
                app.open_create_form(branches, parent);
            }
            SwitcherConfirm::Switched | SwitcherConfirm::NoOp => {
            }
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
    action: &NewWorktreeAction,
) -> Result<std::path::PathBuf, String> {
    use BranchSpec;
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

fn launch_message(rejection: LaunchRejection) -> String {
    use LaunchRejection::*;
    match rejection {
        NoExecutor => "no backend wired into the TUI".into(),
        AlreadyRunning => "another run is in progress".into(),
        NotRunnable(msg) => msg,
    }
}

fn enter_terminal(title: &str) -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut out = stdout();
    // Mouse capture is on so the diff view can react to scroll-wheel
    // events. Drag-selection of text doesn't really work in a
    // side-by-side TUI anyway (the terminal selects across pane
    // boundaries), so keeping capture on globally costs nothing.
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
    use crate::tui::views::diff::git::{
        annotate_read_with_diff, enrich_diff_lines, parse_diff_name_status, parse_hunk_header,
        parse_numstat, parse_status_porcelain, resolve_numstat_destination,
    };
    use crate::tui::views::diff::state::{
        DiffLine, DiffLineKind, DiffStatus, ReadLine, ReadLineKind,
    };
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
        // pass their own `[runtime]` block.
        let prefix = if toml.contains("[runtime]") {
            String::new()
        } else {
            String::from("[runtime]\nbackend = \"none\"\n")
        };
        App::new(Arc::new(
            crate::config::parse_str(&format!("{prefix}{toml}")).unwrap(),
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
        assert_eq!(app.view(), View::ControlCenter);
        handle_event(&mut app, press(KeyCode::Char('T'))).await;
        assert_eq!(app.view(), View::Terminals);
    }

    #[tokio::test]
    async fn capital_w_works_from_terminals_view() {
        // Used to be control-center-only; now global.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.switch_view(View::Terminals);
        handle_event(&mut app, press(KeyCode::Char('W'))).await;
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
    }

    #[tokio::test]
    async fn capital_w_works_from_diff_view() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        app.switch_view(View::Diff);
        handle_event(&mut app, press(KeyCode::Char('W'))).await;
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
    }

    #[tokio::test]
    async fn capital_g_switches_to_diff_view() {
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('G'))).await;
        assert_eq!(app.view(), View::Diff);
    }

    #[tokio::test]
    async fn lowercase_g_does_not_switch_views() {
        // Used to switch to diff; now reserved (uppercase only for
        // view changes). Asserts we don't accidentally rewire it.
        let mut app = app_with("[command.x]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('g'))).await;
        assert_eq!(app.view(), View::ControlCenter);
    }

    #[test]
    fn parse_status_porcelain_classifies_each_line() {
        use DiffStatus;
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
        use DiffStatus;
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
        assert_eq!(
            parse_hunk_header("@@ -100 +200 @@ fn foo()"),
            Some((100, 200))
        );
        assert_eq!(parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn enrich_diff_lines_tracks_line_numbers_through_hunks() {
        use DiffLineKind;
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
            .filter(|l| {
                matches!(
                    l.kind,
                    DiffLineKind::Added | DiffLineKind::Removed | DiffLineKind::Context,
                )
            })
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
        app.switch_view(View::Diff);
        handle_event(&mut app, press(KeyCode::Char('C'))).await;
        assert_eq!(app.view(), View::ControlCenter);
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
        assert_eq!(app.mode(), Mode::Palette);
    }

    #[tokio::test]
    async fn slash_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, press(KeyCode::Char('/'))).await;
        assert_eq!(app.mode(), Mode::Palette);
    }

    #[tokio::test]
    async fn ctrl_k_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('k'))).await;
        assert_eq!(app.mode(), Mode::Palette);
    }

    #[tokio::test]
    async fn ctrl_p_opens_palette() {
        let mut app = app_with("[command.a]\nrun = \"true\"\n");
        handle_event(&mut app, ctrl(KeyCode::Char('p'))).await;
        assert_eq!(app.mode(), Mode::Palette);
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
        assert_eq!(app.mode(), Mode::Normal);
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
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.items()[app.selected_index()].name, "migrate");
    }

    // ──────── annotate_read_with_diff ────────
    //
    // Helpers and tests for the read-mode bg classifier. Each test
    // constructs a small synthetic diff and a flat read-line vec,
    // then asserts the annotated output kinds.


    fn dl_h(text: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Header,
            text: text.into(),
            old_lineno: None,
            new_lineno: None,
            spans: vec![],
        }
    }
    fn dl_hunk() -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Hunk,
            text: "@@".into(),
            old_lineno: None,
            new_lineno: None,
            spans: vec![],
        }
    }
    fn dl_ctx(o: u32, n: u32) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Context,
            text: " ".into(),
            old_lineno: Some(o),
            new_lineno: Some(n),
            spans: vec![],
        }
    }
    fn dl_add(n: u32) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Added,
            text: "+".into(),
            old_lineno: None,
            new_lineno: Some(n),
            spans: vec![],
        }
    }
    fn dl_rem(o: u32) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Removed,
            text: "-".into(),
            old_lineno: Some(o),
            new_lineno: None,
            spans: vec![],
        }
    }
    fn rl(n: u32) -> ReadLine {
        ReadLine {
            kind: ReadLineKind::Plain,
            lineno: n,
            text: format!("L{n}"),
            spans: vec![],
        }
    }

    #[test]
    fn annotate_marks_pure_additions_green() {
        // Diff: 3 context lines, add line 2.
        let diff = vec![
            dl_h("--- a"),
            dl_hunk(),
            dl_ctx(1, 1),
            dl_add(2),
            dl_ctx(2, 3),
        ];
        let read = vec![rl(1), rl(2), rl(3)];
        let out = annotate_read_with_diff(read, &diff);
        let kinds: Vec<_> = out.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ReadLineKind::Plain,
                ReadLineKind::Added,
                ReadLineKind::Plain
            ]
        );
    }

    #[test]
    fn annotate_marks_modifications_blue() {
        // Diff: remove line 2, add the replacement at line 2.
        let diff = vec![
            dl_h("--- a"),
            dl_hunk(),
            dl_ctx(1, 1),
            dl_rem(2),
            dl_add(2),
            dl_ctx(3, 3),
        ];
        let read = vec![rl(1), rl(2), rl(3)];
        let out = annotate_read_with_diff(read, &diff);
        let kinds: Vec<_> = out.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ReadLineKind::Plain,
                ReadLineKind::Modified,
                ReadLineKind::Plain
            ]
        );
    }

    #[test]
    fn annotate_inserts_separator_for_pure_deletion() {
        // Diff: original had 4 lines; line 2 was removed. New file
        // has 3 lines (old 1, 3, 4).
        let diff = vec![
            dl_h("--- a"),
            dl_hunk(),
            dl_ctx(1, 1),
            dl_rem(2),
            dl_ctx(3, 2),
            dl_ctx(4, 3),
        ];
        let read = vec![rl(1), rl(2), rl(3)];
        let out = annotate_read_with_diff(read, &diff);
        // Separator should land between new-line 1 and new-line 2.
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].kind, ReadLineKind::Plain);
        assert_eq!(out[0].lineno, 1);
        assert_eq!(out[1].kind, ReadLineKind::Separator { removed: 1 });
        assert_eq!(out[2].kind, ReadLineKind::Plain);
        assert_eq!(out[2].lineno, 2);
        assert_eq!(out[3].kind, ReadLineKind::Plain);
    }

    #[test]
    fn annotate_handles_deletion_before_first_line() {
        // Original lines 1, 2 were removed; new file starts at what
        // used to be line 3.
        let diff = vec![dl_h("--- a"), dl_hunk(), dl_rem(1), dl_rem(2), dl_ctx(3, 1)];
        let read = vec![rl(1)];
        let out = annotate_read_with_diff(read, &diff);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, ReadLineKind::Separator { removed: 2 });
        assert_eq!(out[1].kind, ReadLineKind::Plain);
        assert_eq!(out[1].lineno, 1);
    }

    #[test]
    fn name_status_captures_rename_old_path() {
        let input = "R100\t.scaffl/commands/seed\t.keel/commands/seed\nM\tREADME.md\n";
        let entries = parse_diff_name_status(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, ".keel/commands/seed");
        assert_eq!(
            entries[0].old_path.as_deref(),
            Some(".scaffl/commands/seed")
        );
        assert_eq!(entries[1].path, "README.md");
        assert!(entries[1].old_path.is_none());
    }

    #[test]
    fn numstat_resolves_plain_rename_form() {
        assert_eq!(resolve_numstat_destination("foo => bar"), "bar".to_string());
        assert_eq!(
            resolve_numstat_destination("src/old.rs => src/new.rs"),
            "src/new.rs".to_string()
        );
    }

    #[test]
    fn numstat_resolves_brace_rename_with_common_suffix() {
        // The bug case from the field: `.{scaffl => keel}/commands/seed`.
        assert_eq!(
            resolve_numstat_destination(".{scaffl => keel}/commands/seed"),
            ".keel/commands/seed".to_string()
        );
        assert_eq!(
            resolve_numstat_destination("src/{old => new}/lib.rs"),
            "src/new/lib.rs".to_string()
        );
    }

    #[test]
    fn numstat_resolves_brace_rename_at_either_end() {
        assert_eq!(
            resolve_numstat_destination("{old => new}/path"),
            "new/path".to_string()
        );
        assert_eq!(
            resolve_numstat_destination("path/{old => new}"),
            "path/new".to_string()
        );
    }

    #[test]
    fn numstat_resolves_empty_side_renames() {
        // Renamed-up: `dir/{sub => }/file.rs` should collapse to `dir/file.rs`.
        assert_eq!(
            resolve_numstat_destination("dir/{sub => }/file.rs"),
            "dir/file.rs".to_string()
        );
        // Renamed-into-subdir: `dir/{ => sub}/file.rs` → `dir/sub/file.rs`.
        assert_eq!(
            resolve_numstat_destination("dir/{ => sub}/file.rs"),
            "dir/sub/file.rs".to_string()
        );
    }

    #[test]
    fn numstat_passes_through_non_rename_path() {
        assert_eq!(
            resolve_numstat_destination("src/main.rs"),
            "src/main.rs".to_string()
        );
    }

    #[test]
    fn parse_numstat_picks_destination_for_renamed_file() {
        let input = "5\t0\t.{scaffl => keel}/commands/seed\n9\t9\tREADME.md\n";
        let entries = parse_numstat(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, ".keel/commands/seed");
        assert_eq!(entries[0].additions, 5);
        assert_eq!(entries[0].deletions, 0);
        assert_eq!(entries[1].path, "README.md");
    }

    #[test]
    fn annotate_handles_trailing_deletion() {
        // Last two lines deleted; no surviving line after them.
        let diff = vec![dl_h("--- a"), dl_hunk(), dl_ctx(1, 1), dl_rem(2), dl_rem(3)];
        let read = vec![rl(1)];
        let out = annotate_read_with_diff(read, &diff);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, ReadLineKind::Plain);
        assert_eq!(out[1].kind, ReadLineKind::Separator { removed: 2 });
    }
}
