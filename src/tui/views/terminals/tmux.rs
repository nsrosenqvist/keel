//! Tmux shell-out + parsing for the Terminals view.
//!
//! Everything keel knows about tmux at the process level lives here:
//! the attach handshake, the `list-windows` query + parser, the
//! `capture-pane` preview grab, the version probe used to decide
//! whether the view is even reachable, and the per-app refresh that
//! drives the worker-snapshot loop.

use crate::tui::app::App;
use crate::tui::app::AttachRequest;
use crate::tui::views::terminals::state::TmuxWindow;

/// Drop into a tmux session/window. Idempotent re: session/window
/// creation: missing pieces are spawned before the attach so the
/// caller can fire-and-forget. Also pins a small set of
/// keel-specific options (status bar, detach binding, bell
/// monitoring) so the user always sees consistent UX no matter how
/// global tmux config differs.
pub(crate) async fn attach_tmux(req: &AttachRequest) {
    use tokio::process::Command;
    let has_session = Command::new("tmux")
        .args(["has-session", "-t", &req.session])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    let target = if req.window == "keel-new" {
        // Fresh shell: ensure session, then unconditionally
        // new-window. `-P -F #{window_index}` would let us read
        // the index back, but the simpler path is "spawn it,
        // immediately attach" — tmux focuses the new window when
        // the client connects.
        //
        // When `window_name` is set, pass `-n` so tmux disables
        // automatic-rename for the new window (otherwise the name
        // would track the foreground process — "docker" for
        // devcontainer shells, which is exactly what we want to
        // avoid).
        if !has_session {
            let mut cmd = Command::new("tmux");
            cmd.args(["new-session", "-d", "-s", &req.session]);
            if let Some(name) = req.window_name.as_deref() {
                cmd.args(["-n", name]);
            }
            if let Some(create) = req.create_with.as_deref() {
                cmd.arg(create);
            }
            let _ = cmd.status().await;
        } else {
            let mut cmd = Command::new("tmux");
            cmd.args(["new-window", "-t", &req.session]);
            if let Some(name) = req.window_name.as_deref() {
                cmd.args(["-n", name]);
            }
            if let Some(create) = req.create_with.as_deref() {
                cmd.arg(create);
            }
            let _ = cmd.status().await;
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
    // Annotate the just-created window with any caller-supplied
    // user options. Used by the devcontainer path to tag the window
    // with its workspaceFolder so the sidebar can render the
    // in-container path rather than the docker client's host-side
    // pwd. Best-effort: tmux returns non-zero if the window vanished
    // between create and set, which is OK to ignore — the missing
    // tag just falls back to the historical cwd display.
    if !req.window_options.is_empty() {
        // The current window of the session is the one we just
        // created (tmux focuses it on new-window). Resolve `:` to
        // the active window via `display-message` so we tag the
        // right one without racing the session-exists branch.
        let win_target = if req.window == "keel-new" {
            format!("{}:", req.session)
        } else {
            format!("{}:{}", req.session, req.window)
        };
        for (key, value) in &req.window_options {
            let _ = Command::new("tmux")
                .args(["set-option", "-w", "-t", &win_target, key, value])
                .status()
                .await;
        }
    }
    // Pin options that decide whether ctrl+b d ends up killing
    // the session or its windows. Users with `destroy-unattached
    // on` (or rebound `d` to `kill-session`) in their global
    // tmux config would otherwise see every shell vanish the
    // moment keel detached — definitely not what "detach"
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

    // Turn on per-window bell monitoring so #{window_bell_flag}
    // fires when a program in the window emits BEL. Coding agents
    // (Claude Code, codex, …) ring the terminal bell to grab the
    // user's attention; the Terminals list surfaces those bells as
    // a per-row indicator. tmux auto-clears the flag when the
    // window becomes current, so attaching doubles as dismissal.
    //
    // Scope is per-session: each existing window gets the option
    // set explicitly, and an `after-new-window` hook keeps future
    // windows (including those the user creates via ctrl+b c
    // outside keel) in sync. Nothing global on the server.
    if let WindowList::Ok(windows) = list_tmux_windows(&req.session).await {
        for w in &windows {
            let target = format!("{}:{}", req.session, w.index);
            let _ = Command::new("tmux")
                .args(["set-window-option", "-t", &target, "monitor-bell", "on"])
                .status()
                .await;
        }
    }
    let _ = Command::new("tmux")
        .args([
            "set-hook",
            "-t",
            &req.session,
            "after-new-window",
            "set-window-option monitor-bell on",
        ])
        .status()
        .await;

    // Inject the keel-flavoured status bar so users always see
    // the detach hint. Mirrors AOE's layout: session name styled
    // on the left, then a separator and the detach instruction;
    // tmux's window list rides on the right of `status-left` as
    // its built-in `status-window-format` output.
    //
    // Set on every attach: idempotent (overwrite-on-set), and
    // free of "did the user customise tmux globally?" assumptions
    // — we only touch this one session, leaving global config
    // alone.
    // Color schema mirrors the terminals view in the TUI: accent
    // `colour79` (mint teal, matches `view_accent(View::Terminals)`),
    // muted foreground, and the terminal's default background so the
    // bar blends in instead of wearing tmux's loud default green.
    let status_left =
        " #[fg=colour79,bold]#S#[default] #[fg=brightblack]│#[default] ctrl+b d to detach ";
    for arg_set in [
        [
            "set-option",
            "-t",
            &req.session,
            "status-style",
            "bg=default,fg=colour250",
        ],
        ["set-option", "-t", &req.session, "status-left", status_left],
        ["set-option", "-t", &req.session, "status-left-length", "60"],
        [
            "set-option",
            "-t",
            &req.session,
            "window-status-current-style",
            "fg=colour79,bold",
        ],
        [
            "set-option",
            "-t",
            &req.session,
            "window-status-style",
            "fg=colour250",
        ],
    ] {
        let _ = Command::new("tmux").args(arg_set).status().await;
    }
    // Attach. Inherits stdio.
    let _ = Command::new("tmux")
        .args(["attach", "-t", &target])
        .status()
        .await;
}

/// Outcome of asking tmux for the windows in a session. Lets the
/// caller distinguish "no such session" from "session has no
/// windows" — which look identical when collapsed to a Vec.
pub enum WindowList {
    Ok(Vec<TmuxWindow>),
    NoSession(String),
    SpawnFailed(String),
}

/// Query tmux for the current window list of `session`. Format
/// uses `\t` between fields so window names / paths with spaces
/// or colons round-trip cleanly.
pub async fn list_tmux_windows(session: &str) -> WindowList {
    use tokio::process::Command;
    let output = match Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index}\t#{window_name}\t#{pane_current_path}\t#{window_bell_flag}\t#{@keel_workspace}",
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
pub(crate) async fn capture_pane(session: &str, window: u32) -> Vec<String> {
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
/// for. Lines look like `0\tzsh\t/home/me/proj\t1\t` or
/// `1\tdc\t/home/me\t0\t/workspaces/foo`. The path may be empty for
/// windows that haven't launched a process yet — preserved as `None`.
/// The bell column is `1` when set, empty or `0` otherwise; missing
/// entirely on older tmux versions, in which case it's treated as
/// cleared. The `@keel_workspace` column is empty for any window
/// keel didn't tag (host shells, services, pre-existing windows).
/// Public for tests.
pub fn parse_tmux_windows(input: &str) -> Vec<TmuxWindow> {
    let mut out = Vec::new();
    for line in input.lines() {
        let mut parts = line.splitn(5, '\t');
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
        let has_bell = matches!(parts.next(), Some("1"));
        let workspace = parts.next().map(str::to_string).filter(|s| !s.is_empty());
        out.push(TmuxWindow {
            index,
            name: name.to_string(),
            cwd,
            has_bell,
            workspace,
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
///   - NoSession after the user just attached → flash a short
///     hint. Usually the user killed the session themselves
///     (ctrl+d on the last window); no diagnostic dump.
///   - SpawnFailed → flash the spawn error.
pub(crate) async fn refresh_tmux_windows(app: &mut App, expecting_session: bool) {
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
                app.terminals_mut().set_preview(*idx, lines);
            }
            app.terminals_set_windows(w);
        }
        WindowList::NoSession(_msg) => {
            app.terminals_set_windows(Vec::new());
            if expecting_session {
                // Usually a user-initiated detach (ctrl+d on the
                // last window). Surface a short hint but skip the
                // tmux stderr / diagnostic dump — it's not a bug.
                app.flash = Some(format!("tmux session `{session}` ended"));
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

/// One-shot tmux availability probe. Records the result on
/// [`crate::tui::views::terminals::state::TerminalsView`] so the
/// rest of the TUI can decide whether the terminals view is
/// reachable. No-op after the first call (the probe is cached).
pub(crate) async fn ensure_tmux_probed(app: &mut App) {
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
    app.terminals_mut().set_tmux_available(ok);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tmux_windows_handles_typical_output() {
        let input = "0\tzsh\t/home/me/proj\t0\n1\tsvc:app\t/\t0\n2\tvim\t/home/me/proj/src\t0\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].index, 0);
        assert_eq!(windows[0].name, "zsh");
        assert_eq!(windows[0].cwd.as_deref(), Some("/home/me/proj"));
        assert!(!windows[0].has_bell);
        assert_eq!(windows[1].name, "svc:app");
        assert_eq!(windows[1].cwd.as_deref(), Some("/"));
        assert_eq!(windows[2].index, 2);
        assert_eq!(windows[2].cwd.as_deref(), Some("/home/me/proj/src"));
    }

    #[test]
    fn parse_tmux_windows_handles_missing_cwd() {
        let input = "0\tzsh\t\t0\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].name, "zsh");
        assert!(windows[0].cwd.is_none());
        assert!(!windows[0].has_bell);
    }

    #[test]
    fn parse_tmux_windows_reads_bell_flag() {
        let input = "0\tzsh\t/home/me\t1\n1\tvim\t/home/me\t0\n2\tnvim\t/home/me\t\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 3);
        assert!(windows[0].has_bell);
        assert!(!windows[1].has_bell);
        assert!(!windows[2].has_bell);
    }

    #[test]
    fn parse_tmux_windows_tolerates_missing_bell_column() {
        let input = "0\tzsh\t/home/me\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 1);
        assert!(!windows[0].has_bell);
    }

    #[test]
    fn parse_tmux_windows_skips_malformed_lines() {
        let input = "0\tzsh\t/home/me\t0\nnot a window\n2\tvim\t/home/me/src\t0\n";
        let windows = parse_tmux_windows(input);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].name, "zsh");
        assert_eq!(windows[1].name, "vim");
    }
}
