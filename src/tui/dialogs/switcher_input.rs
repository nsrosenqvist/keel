//! Key + mouse dispatch for the worktree switcher (list + create form).

use crate::tui::app::{App, ClickTarget};
use crate::tui::dialogs::switcher::{BranchSpec, NewWorktreeAction, SwitcherConfirm};
use crate::tui::terminal::{ClickKind, hit_test, resolve_click};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub async fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit();
        return;
    }
    // Two sub-modes inside this modal: list-of-worktrees (default)
    // and the new-worktree create form. The form takes over key
    // dispatch when active.
    if app.switcher().and_then(|s| s.creating.as_ref()).is_some() {
        handle_form_key(app, code).await;
        return;
    }
    match code {
        KeyCode::Esc => app.close_switcher(),
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(s) = app.switcher_mut() {
                s.select_prev();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(s) = app.switcher_mut() {
                s.select_next();
            }
        }
        KeyCode::Enter => activate_selection(app).await,
        _ => {}
    }
}

async fn handle_form_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.switcher_form_cancel(),
        KeyCode::Tab => {
            if let Some(f) = app.switcher_form_mut() {
                f.toggle_focus();
            }
        }
        KeyCode::Up => {
            if let Some(f) = app.switcher_form_mut() {
                f.select_prev();
            }
        }
        KeyCode::Down => {
            if let Some(f) = app.switcher_form_mut() {
                f.select_next();
            }
        }
        KeyCode::Backspace => {
            if let Some(f) = app.switcher_form_mut() {
                f.pop_char();
            }
        }
        KeyCode::Enter => {
            // Resolve the form into (path, BranchSpec); shell out
            // to git; report back via switcher_form_finish.
            let Some(action) = app.switcher_form().and_then(|f| f.resolve()) else {
                return;
            };
            let project_root = app.project_root().to_path_buf();
            let result = create_worktree(&project_root, &action).await;
            app.switcher_form_finish(result);
        }
        KeyCode::Char(c) => {
            if let Some(f) = app.switcher_form_mut() {
                f.push_char(c);
            }
        }
        _ => {}
    }
}

/// Click → select; double-click → switch / open the new-worktree
/// form (the sentinel row at the end of the entries list).
pub async fn handle_mouse(app: &mut App, me: MouseEvent) {
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
                ClickKind::Select => {
                    if let Some(s) = app.switcher_mut() {
                        s.select_at(idx);
                    }
                }
                ClickKind::Activate => {
                    if let Some(s) = app.switcher_mut() {
                        s.select_at(idx);
                    }
                    activate_selection(app).await;
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(s) = app.switcher_mut() {
                s.select_next();
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(s) = app.switcher_mut() {
                s.select_prev();
            }
        }
        _ => {}
    }
}

/// Resolve the switcher's selected row. Mirrors the
/// `KeyCode::Enter` arm of [`handle_key`] — including the async
/// branch list / toplevel fetch for the "+ new worktree" sentinel.
async fn activate_selection(app: &mut App) {
    match app.switcher_confirm() {
        SwitcherConfirm::OpenCreateForm => {
            let project_root = app.project_root().to_path_buf();
            let branches = crate::runtime::list_branches(&project_root).await;
            // Anchor new worktrees against the git toplevel's
            // parent so they land next to the repo no matter
            // where ampelos was invoked from (e.g. running in
            // `<repo>/tmp/test` shouldn't push them into tmp/).
            let parent = crate::runtime::git_toplevel(&project_root)
                .await
                .and_then(|tl| tl.parent().map(|p| p.to_path_buf()));
            app.open_create_form(branches, parent);
        }
        SwitcherConfirm::Switched | SwitcherConfirm::NoOp => {}
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
