//! Lazygit handoff: suspend the TUI, run lazygit inheriting the
//! terminal, resume on exit.
//!
//! Same pattern editors-from-TUIs use everywhere. Lazygit runs as a
//! plain foreground child process — no tmux, no PTY juggling, no
//! flicker beyond the suspend/resume frame. The actual
//! `LeaveAlternateScreen` / `EnterAlternateScreen` dance lives in
//! the event loop (next to the tmux-attach handoff that does the
//! same thing) so this module can stay free of crossterm specifics.

use std::path::Path;
use tokio::process::Command;

/// Run lazygit in `project_root`, inheriting stdio. Returns when the
/// user quits lazygit (or it fails to start). Caller is responsible
/// for suspending / resuming the alternate screen around this call.
pub async fn run(project_root: &Path) -> Result<(), String> {
    let status = Command::new("lazygit")
        .current_dir(project_root)
        .status()
        .await
        .map_err(|e| format!("lazygit spawn failed: {e}"))?;
    if !status.success() {
        // Lazygit exits non-zero when the user pipes it nonsense or
        // when git itself errors out; treat that as informational
        // rather than a hard failure. The user already saw the error
        // on lazygit's screen.
        return Ok(());
    }
    Ok(())
}

/// Best-effort check for `lazygit` on PATH. Resolves once at TUI
/// startup; the `L` keybind is a no-op (with a hint) when the binary
/// isn't there.
pub fn is_available() -> bool {
    which::which("lazygit").is_ok()
}
