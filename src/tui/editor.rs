//! Open files / the worktree in the user's preferred editor.
//!
//! Two independent axes drive launch behaviour:
//!
//! - **Launch mode** ([`LaunchMode`]): terminal editors need the TUI
//!   to step aside (leave alt screen, run the child with inherited
//!   stdio, re-enter), the same handoff lazygit uses. GUI editors
//!   want a fire-and-forget spawn so the TUI stays painted.
//! - **Directory support** (`opens_directory`): can the binary accept
//!   a directory as its target? VS Code / IntelliJ / vim's netrw say
//!   yes; nano / kakoune / gedit say no. Independent of the launch
//!   mode — vim is a terminal editor that *does* open directories,
//!   gedit is a GUI editor that *doesn't*.
//!
//! Neither dimension is reliably detectable at runtime, so we ship a
//! small registry of well-known editors and let the user override
//! both via `[editor] terminal = ...` and `[editor] opens_directory
//! = ...`.

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
    /// True when the binary opens a directory sensibly — drives the
    /// `E` (open worktree root) keybind's visibility and dispatch
    /// gate. Independent of [`Self::mode`].
    pub opens_directory: bool,
}

impl ResolvedEditor {
    /// Human-readable binary name for flash messages.
    pub fn display_name(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("editor")
    }
}

/// Built-in registry entry: `(binary_name, mode, opens_directory)`.
///
/// **Launch mode** controls whether the TUI suspends. **opens_directory**
/// controls whether `E` (open worktree root) is offered.
///
/// Unknown editors default to terminal mode + `opens_directory = false`
/// — POSIX `$EDITOR` is conventionally a TTY editor that opens single
/// files. The `E` keybind stays hidden in that case until the user
/// opts in via `[editor] opens_directory = true`.
const REGISTRY: &[(&str, LaunchMode, bool)] = &[
    // Terminal editors that open directories (project-aware via
    // netrw / dired / built-in file browser).
    ("vim", LaunchMode::Terminal, true),
    ("nvim", LaunchMode::Terminal, true),
    ("emacs", LaunchMode::Terminal, true),
    ("hx", LaunchMode::Terminal, true),
    ("helix", LaunchMode::Terminal, true),
    ("micro", LaunchMode::Terminal, true),
    // Terminal editors that don't open directories.
    ("vi", LaunchMode::Terminal, false),
    ("nano", LaunchMode::Terminal, false),
    ("kak", LaunchMode::Terminal, false),
    ("kakoune", LaunchMode::Terminal, false),
    ("mcedit", LaunchMode::Terminal, false),
    ("ed", LaunchMode::Terminal, false),
    // GUI editors / IDEs that open directories as projects.
    ("code", LaunchMode::Gui, true),
    ("code-insiders", LaunchMode::Gui, true),
    ("codium", LaunchMode::Gui, true),
    ("cursor", LaunchMode::Gui, true),
    ("subl", LaunchMode::Gui, true),
    ("sublime_text", LaunchMode::Gui, true),
    ("kate", LaunchMode::Gui, true),
    ("gvim", LaunchMode::Gui, true),
    ("mvim", LaunchMode::Gui, true),
    ("nvim-qt", LaunchMode::Gui, true),
    ("idea", LaunchMode::Gui, true),
    ("pycharm", LaunchMode::Gui, true),
    ("webstorm", LaunchMode::Gui, true),
    ("goland", LaunchMode::Gui, true),
    ("clion", LaunchMode::Gui, true),
    ("rustrover", LaunchMode::Gui, true),
    ("phpstorm", LaunchMode::Gui, true),
    ("rubymine", LaunchMode::Gui, true),
    ("zed", LaunchMode::Gui, true),
    // GUI editors that don't really open directories — they're
    // file-pickers wearing a window.
    ("gedit", LaunchMode::Gui, false),
];

fn lookup(binary: &str) -> Option<(LaunchMode, bool)> {
    let key = std::path::Path::new(binary)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(binary)
        .to_ascii_lowercase();
    REGISTRY
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, mode, dir)| (*mode, *dir))
}

/// Resolve the editor command. Source order:
///
///   1. `[editor] command = "..."` in `croft.toml`
///   2. `$VISUAL`
///   3. `$EDITOR`
///   4. literal `"vim"`
///
/// Launch mode comes from `[editor] terminal = ...` if set, else the
/// registry; directory support comes from `[editor] opens_directory =
/// ...` if set, else the registry. Unknown editors default to
/// terminal + no-dir.
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

    let (default_mode, default_dir) = lookup(&argv[0]).unwrap_or((LaunchMode::Terminal, false));
    let mode = match config.terminal {
        Some(true) => LaunchMode::Terminal,
        Some(false) => LaunchMode::Gui,
        None => default_mode,
    };
    let opens_directory = config.opens_directory.unwrap_or(default_dir);

    ResolvedEditor {
        argv,
        mode,
        opens_directory,
    }
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
            opens_directory: None,
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.argv, vec!["nano", "--restricted"]);
        assert_eq!(resolved.mode, LaunchMode::Terminal);
        assert!(!resolved.opens_directory, "nano cannot open dirs");
    }

    #[test]
    fn lookup_known_terminal_editor_with_dir() {
        let (mode, dir) = lookup("vim").unwrap();
        assert_eq!(mode, LaunchMode::Terminal);
        assert!(dir, "vim opens directories via netrw");
        let (mode, dir) = lookup("/usr/bin/nvim").unwrap();
        assert_eq!(mode, LaunchMode::Terminal);
        assert!(dir);
        let (mode, dir) = lookup("HX").unwrap();
        assert_eq!(mode, LaunchMode::Terminal);
        assert!(dir);
    }

    #[test]
    fn lookup_known_terminal_editor_without_dir() {
        let (mode, dir) = lookup("nano").unwrap();
        assert_eq!(mode, LaunchMode::Terminal);
        assert!(!dir);
    }

    #[test]
    fn lookup_known_gui_editor_with_dir() {
        let (mode, dir) = lookup("code").unwrap();
        assert_eq!(mode, LaunchMode::Gui);
        assert!(dir);
        let (mode, dir) = lookup("/snap/bin/cursor").unwrap();
        assert_eq!(mode, LaunchMode::Gui);
        assert!(dir);
    }

    #[test]
    fn lookup_known_gui_editor_without_dir() {
        let (mode, dir) = lookup("gedit").unwrap();
        assert_eq!(mode, LaunchMode::Gui);
        assert!(!dir);
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("my-unknown-editor").is_none());
    }

    #[test]
    fn config_overrides_win() {
        // Terminal-mode override on a GUI editor:
        let cfg = EditorConfig {
            command: Some("code --wait".into()),
            terminal: Some(true),
            opens_directory: None,
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.mode, LaunchMode::Terminal);
        assert!(resolved.opens_directory, "registry says code opens dirs");

        // GUI + dir override on an unknown editor:
        let cfg = EditorConfig {
            command: Some("my-custom-editor".into()),
            terminal: Some(false),
            opens_directory: Some(true),
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.mode, LaunchMode::Gui);
        assert!(resolved.opens_directory);
    }

    #[test]
    fn unknown_editor_defaults_to_terminal_no_dir() {
        let cfg = EditorConfig {
            command: Some("my-unknown-editor".into()),
            terminal: None,
            opens_directory: None,
        };
        let resolved = resolve(&cfg);
        assert_eq!(resolved.mode, LaunchMode::Terminal);
        assert!(!resolved.opens_directory);
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
        assert!(resolved.opens_directory);
    }
}
