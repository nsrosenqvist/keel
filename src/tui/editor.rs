//! Open files / the worktree in the user's preferred editor.
//!
//! Two flavours of editor exist and they want different launch
//! semantics:
//!
//! - **Terminal editors** (vim, nvim, nano, ...) need the TUI to step
//!   aside the way it does for lazygit — leave the alternate screen,
//!   let the child inherit stdio, re-enter and redraw on exit.
//! - **GUI editors** (code, cursor, idea, ...) want a fire-and-forget
//!   spawn: the binary returns immediately (the real window is already
//!   open via IPC) and the TUI should keep painting underneath.
//!
//! Reliable runtime detection of "is this binary a GUI?" doesn't
//! exist — many editors have both modes (`code` vs `code-tunnel`,
//! `nvim` vs `nvim-qt`). So we ship a small registry of well-known
//! editors and let the user override via `[editor] terminal = ...`.

use crate::config::EditorConfig;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Whether to suspend the TUI around the editor launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Terminal editor: caller must leave the alternate screen, run
    /// the child with inherited stdio, then re-enter.
    Terminal,
    /// GUI editor: fire-and-forget spawn, TUI stays painted.
    Gui,
}

/// A resolved editor command + how to launch it. Built once at TUI
/// startup and stashed on `App`.
#[derive(Debug, Clone)]
pub struct ResolvedEditor {
    /// Parsed command line. `argv[0]` is the binary; remaining entries
    /// are arguments inserted *before* the target path on each launch.
    pub argv: Vec<String>,
    pub mode: LaunchMode,
}

impl ResolvedEditor {
    /// Human-readable binary name for flash messages.
    pub fn display_name(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("editor")
    }
}

/// Built-in registry. Lowercase binary name → launch mode. Unknown
/// editors default to [`LaunchMode::Terminal`] — POSIX `$EDITOR` is
/// conventionally a TTY editor, and suspending the TUI is the safer
/// default (worst case: the user sees a quick blink; best case: their
/// editor actually works).
const REGISTRY: &[(&str, LaunchMode)] = &[
    // Terminal editors
    ("vi", LaunchMode::Terminal),
    ("vim", LaunchMode::Terminal),
    ("nvim", LaunchMode::Terminal),
    ("nano", LaunchMode::Terminal),
    ("hx", LaunchMode::Terminal),
    ("helix", LaunchMode::Terminal),
    ("micro", LaunchMode::Terminal),
    ("kak", LaunchMode::Terminal),
    ("kakoune", LaunchMode::Terminal),
    ("mcedit", LaunchMode::Terminal),
    ("emacs", LaunchMode::Terminal),
    ("ed", LaunchMode::Terminal),
    // GUI editors / IDEs
    ("code", LaunchMode::Gui),
    ("code-insiders", LaunchMode::Gui),
    ("codium", LaunchMode::Gui),
    ("cursor", LaunchMode::Gui),
    ("subl", LaunchMode::Gui),
    ("sublime_text", LaunchMode::Gui),
    ("gedit", LaunchMode::Gui),
    ("kate", LaunchMode::Gui),
    ("gvim", LaunchMode::Gui),
    ("mvim", LaunchMode::Gui),
    ("nvim-qt", LaunchMode::Gui),
    ("idea", LaunchMode::Gui),
    ("pycharm", LaunchMode::Gui),
    ("webstorm", LaunchMode::Gui),
    ("goland", LaunchMode::Gui),
    ("clion", LaunchMode::Gui),
    ("rustrover", LaunchMode::Gui),
    ("phpstorm", LaunchMode::Gui),
    ("rubymine", LaunchMode::Gui),
    ("zed", LaunchMode::Gui),
];

fn classify(binary: &str) -> LaunchMode {
    let key = std::path::Path::new(binary)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(binary)
        .to_ascii_lowercase();
    REGISTRY
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, mode)| *mode)
        .unwrap_or(LaunchMode::Terminal)
}

/// Resolve the editor command. Source order:
///
///   1. `[editor] command = "..."` in `keel.toml`
///   2. `$VISUAL`
///   3. `$EDITOR`
///   4. literal `"vim"`
///
/// The launch mode comes from `[editor] terminal = ...` if set, else
/// from the registry lookup on `argv[0]`.
pub fn resolve(config: &EditorConfig) -> ResolvedEditor {
    let raw = config
        .command
        .as_deref()
        .map(str::to_owned)
        .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "vim".to_owned());

    // shell-style split is overkill — editors never quote arguments
    // in $EDITOR. Whitespace tokenisation matches what `sh -c "$EDITOR
    // file"` would produce for the typical `code --wait` / `emacs -nw`
    // style strings.
    let argv: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    let argv = if argv.is_empty() {
        vec!["vim".to_owned()]
    } else {
        argv
    };

    let mode = match config.terminal {
        Some(true) => LaunchMode::Terminal,
        Some(false) => LaunchMode::Gui,
        None => classify(&argv[0]),
    };

    ResolvedEditor { argv, mode }
}

/// Run a terminal editor with inherited stdio. Caller is responsible
/// for suspending / resuming the alternate screen — same contract as
/// the lazygit handoff in `crate::tui::lazygit::run`.
pub async fn run_terminal(
    project_root: &Path,
    editor: &ResolvedEditor,
    target: &Path,
) -> Result<(), String> {
    let mut cmd = Command::new(&editor.argv[0]);
    cmd.args(&editor.argv[1..])
        .arg(target)
        .current_dir(project_root);
    let status = cmd
        .status()
        .await
        .map_err(|e| format!("{} spawn failed: {e}", editor.display_name()))?;
    if !status.success() {
        // Terminal editors return non-zero for all sorts of
        // user-driven reasons (`:cq` in vim, signal-on-exit). The user
        // already saw whatever happened on their own screen — treat
        // it as informational, mirroring the lazygit handoff.
        return Ok(());
    }
    Ok(())
}

/// Spawn a GUI editor detached. Stdio is nulled so the child can't
/// scribble over the TUI between frames; we never await it.
pub fn spawn_gui(
    project_root: &Path,
    editor: &ResolvedEditor,
    target: &Path,
) -> Result<(), String> {
    let mut cmd = Command::new(&editor.argv[0]);
    cmd.args(&editor.argv[1..])
        .arg(target)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("{} spawn failed: {e}", editor.display_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> EditorConfig {
        EditorConfig::default()
    }

    #[test]
    fn config_command_wins_over_env() {
        // SAFETY: tests run single-threaded under cargo by default in
        // this crate; if that changes, this needs a serial guard.
        // We don't depend on $EDITOR being set externally for this
        // assertion — the config branch fires before env lookup.
        let cfg = EditorConfig {
            command: Some("nano --restricted".into()),
            terminal: None,
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.argv, vec!["nano", "--restricted"]);
        assert_eq!(resolved.mode, LaunchMode::Terminal);
    }

    #[test]
    fn classify_known_terminal_editor() {
        assert_eq!(classify("vim"), LaunchMode::Terminal);
        assert_eq!(classify("/usr/bin/nvim"), LaunchMode::Terminal);
        assert_eq!(classify("HX"), LaunchMode::Terminal);
    }

    #[test]
    fn classify_known_gui_editor() {
        assert_eq!(classify("code"), LaunchMode::Gui);
        assert_eq!(classify("/snap/bin/cursor"), LaunchMode::Gui);
    }

    #[test]
    fn classify_unknown_defaults_to_terminal() {
        assert_eq!(classify("my-unknown-editor"), LaunchMode::Terminal);
    }

    #[test]
    fn config_terminal_override_wins() {
        let cfg = EditorConfig {
            command: Some("code --wait".into()),
            terminal: Some(true),
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.mode, LaunchMode::Terminal);

        let cfg = EditorConfig {
            command: Some("my-unknown-editor".into()),
            terminal: Some(false),
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.mode, LaunchMode::Gui);
    }

    #[test]
    fn fallback_is_vim() {
        // Empty config + nothing reasonable in env → vim.
        // We can't safely clear the parent process's env in a unit
        // test, so this only proves the *empty-string* env case.
        // SAFETY: setting/unsetting env in tests is not thread-safe
        // (libstd flagged it as `unsafe` in 1.84). Best-effort —
        // accept that another test in the same process may overlap.
        unsafe {
            std::env::set_var("VISUAL", "");
            std::env::set_var("EDITOR", "");
        }
        let resolved = resolve(&empty());
        assert_eq!(resolved.argv, vec!["vim"]);
        assert_eq!(resolved.mode, LaunchMode::Terminal);
    }
}
