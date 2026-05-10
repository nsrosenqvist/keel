//! TUI application state.
//!
//! The model and the controller. Pure functions — no terminal I/O here.

use crate::palette::Palette;
use crate::runner::RunState;
use crate::services::ServicePane;
use crate::watchers::WatcherPane;
use scaffl_config::{Config, Recipe, ScriptCommand, model::UiPane};
use scaffl_container::Backend;
use scaffl_runtime::Executor;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Top-level view of the TUI. Orthogonal to [`Mode`]: a view picks
/// what's *rendered*, a mode picks how *keys* route. The control
/// center is today's home view; terminals (tmux-backed shells) and
/// diff (file-level git review) are second-class views reachable by
/// `T` / `g` and back via `c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The home dashboard: services, watchers, recipes, scripts, and
    /// the container row in one place. The "command center" of the
    /// project — hence the `c` keybind to come back.
    ControlCenter,
    Terminals,
    Diff,
}

impl View {
    /// Short tag used in the status bar.
    pub fn tag(self) -> &'static str {
        match self {
            View::ControlCenter => "control",
            View::Terminals => "terminals",
            View::Diff => "diff",
        }
    }
}

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
    /// The container backend itself — a single synthetic row that
    /// hosts compose lifecycle output (`U` / `D` / `R` / `S`). One
    /// such row exists when the configured backend is non-`none`.
    Container,
    Service,
    Watcher,
    Recipe,
    Script,
}

/// A single sidebar entry.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
}

/// Why a run attempt was rejected.
#[derive(Debug, Clone)]
pub enum LaunchRejection {
    NoExecutor,
    AlreadyRunning,
    NotRunnable(String),
}

/// High-level UI mode. Controls how key events route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Palette,
    /// A modal confirmation is open — keys route to its dialog.
    Confirm,
    /// An args prompt is open for a `forward_args = true` row.
    ArgsPrompt,
    /// The worktree switcher is open — keys navigate the list or
    /// edit the new-worktree form when active.
    WorktreeSwitcher,
}

/// Composite key for the per-row run map. Recipe / Script names can
/// theoretically collide with service / watcher names, so the kind
/// is part of the key.
pub type RunKey = (ItemKind, String);

/// Pending decision when the user tries to launch a running command.
/// Kept simple: only one in-flight question at a time, only one kind
/// of question (kill-and-restart). New question shapes get added here
/// when they arrive.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub body: String,
    /// Currently-focused choice. `true` = Yes (the default).
    pub yes_focused: bool,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Abort the named run and relaunch it.
    KillAndRestart { key: RunKey },
}

/// Open args prompt for a `forward_args = true` row. Only one is
/// open at a time; selection is locked while the prompt is up.
#[derive(Debug, Clone)]
pub struct ArgsPrompt {
    pub item_name: String,
    pub kind: ItemKind,
    pub input: String,
}

/// One row in the worktree switcher list. Slug is computed by the
/// runtime crate; `is_current` flags the worktree scaffl is
/// currently bound to so we can render it differently.
#[derive(Debug, Clone)]
pub struct WorktreeRow {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub slug: String,
    pub is_current: bool,
}

/// State for the Terminals view (`T`). Tmux-backed; one session per
/// worktree, plus arbitrary user-named windows.
#[derive(Debug, Clone)]
pub struct TerminalsState {
    /// `None` until probed; `Some(false)` when tmux is missing.
    pub tmux_available: Option<bool>,
    /// `scaffl-<project>-<slug>` — stable per worktree.
    pub session_name: String,
    /// User-defined terminals in this session. Service-attach rows
    /// don't live here; they're synthesised from `App::services` at
    /// render time so they always reflect the current service list.
    pub terminals: Vec<TerminalRow>,
    /// Selected index across the (services + terminals + sentinel)
    /// concatenation that the renderer / keymap iterate.
    pub selected: usize,
    /// Open new-terminal form, if any.
    pub creating: Option<NewTerminalForm>,
}

#[derive(Debug, Clone)]
pub struct TerminalRow {
    pub name: String,
    pub kind: TerminalKind,
}

#[derive(Debug, Clone)]
pub enum TerminalKind {
    /// Plain host shell; opens `cd <project> && exec $SHELL` in the
    /// tmux window.
    Shell,
    /// `docker compose exec -it <svc> $SHELL` inside the named
    /// service. Synthetic — only constructed for the attach action,
    /// not stored in `terminals`.
    ServiceShell { service: String },
    /// User-defined window with a custom command line.
    Custom { command: String },
}

#[derive(Debug, Clone)]
pub struct NewTerminalForm {
    pub name_input: String,
    pub command_input: String,
    pub focus: NewTerminalField,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewTerminalField {
    Name,
    Command,
}

/// One visible row in the Terminals view's sidebar. The view
/// renders services first, then custom terminals, then the
/// `+ new` sentinel.
#[derive(Debug, Clone)]
pub enum TerminalsRow {
    Service(String),
    Terminal(TerminalRow),
    NewSentinel,
}

/// Single-quote a path for safe shell embedding (tmux send-window
/// commands run through `sh -c`).
fn shell_escape(s: String) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

impl TerminalsState {
    /// Build initial state for a project. Pre-populates a single
    /// `shell` terminal so the view always has at least one row to
    /// attach to. Session name is derived from the project name +
    /// any worktree slug — but slug-aware naming requires the
    /// runtime identity, so we accept just the project name here
    /// and the App caller patches the session name in
    /// [`App::with_project_root`] when more is known.
    pub fn default_for(config: &Config) -> Self {
        let project = config.project.name.as_deref().unwrap_or("scaffl");
        Self {
            tmux_available: None,
            session_name: format!("scaffl-{project}"),
            terminals: vec![TerminalRow {
                name: "shell".into(),
                kind: TerminalKind::Shell,
            }],
            selected: 0,
            creating: None,
        }
    }
}

/// State for the Diff view (`g`). Populated lazily on first
/// switch, rebuilt on `r`. Per-file diff bodies cache so
/// navigating among files doesn't re-shell-out to git.
#[derive(Debug, Clone, Default)]
pub struct DiffState {
    pub files: Vec<DiffFile>,
    pub selected: usize,
    pub cache: std::collections::HashMap<String, Vec<DiffLine>>,
    /// True once `files` has been populated at least once. Lets the
    /// renderer distinguish "no changes" from "haven't checked yet".
    pub loaded: bool,
    /// Last error from `git status` / `git diff`, if any.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub status: DiffStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Other,
}

impl DiffStatus {
    pub fn letter(self) -> char {
        match self {
            DiffStatus::Modified => 'M',
            DiffStatus::Added => 'A',
            DiffStatus::Deleted => 'D',
            DiffStatus::Renamed => 'R',
            DiffStatus::Untracked => 'U',
            DiffStatus::Other => '?',
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Added,
    Removed,
    Context,
    Hunk,
    Header,
}

impl DiffLineKind {
    pub fn classify(line: &str) -> Self {
        if line.starts_with("@@") {
            return DiffLineKind::Hunk;
        }
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("Binary files")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
        {
            return DiffLineKind::Header;
        }
        if line.starts_with('+') {
            return DiffLineKind::Added;
        }
        if line.starts_with('-') {
            return DiffLineKind::Removed;
        }
        DiffLineKind::Context
    }
}

/// One-shot signal from the Terminals view to the event loop:
/// "leave the alternate screen, attach to this tmux window, come
/// back when the user detaches." Created (or, if needed, sent
/// alongside a window-creation command) when the user hits enter
/// on a terminal row.
#[derive(Debug, Clone)]
pub struct AttachRequest {
    pub session: String,
    pub window: String,
    /// When set, the attach handler runs this shell command first
    /// to create the window. `None` means the window already exists.
    pub create_with: Option<String>,
}

/// Sub-state for the "create new worktree" flow inside the
/// switcher modal.
#[derive(Debug, Clone)]
pub struct NewWorktreeForm {
    /// Path where the new worktree directory will be created.
    /// Prefilled with `<parent-of-current>/<branch-input>` so
    /// the user only really needs to type the branch.
    pub path_input: String,
    /// Branch to attach. Empty + path naming an existing branch
    /// → use existing; non-empty → `git worktree add -b <branch>`.
    pub branch_input: String,
    pub focus: NewFormField,
    /// Last error from `git worktree add`, if any. Surfaces as a
    /// hint inside the modal so the user can fix and retry without
    /// the modal closing.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewFormField {
    Path,
    Branch,
}

/// Whole switcher state. The list always has a sentinel "+ new
/// worktree" row at the end; selecting it opens `creating`.
#[derive(Debug, Clone)]
pub struct WorktreeSwitcher {
    pub entries: Vec<WorktreeRow>,
    pub selected: usize,
    pub creating: Option<NewWorktreeForm>,
}

impl WorktreeSwitcher {
    /// Index of the synthetic "+ new worktree" row — always last.
    pub fn new_row_index(&self) -> usize {
        self.entries.len()
    }
    /// Total rows including the new-worktree sentinel.
    pub fn total_rows(&self) -> usize {
        self.entries.len() + 1
    }
}

/// TUI application state.
pub struct App {
    config: Arc<Config>,
    items: Vec<Item>,
    selected: usize,
    quit: bool,
    executor: Option<Executor>,
    backend: Option<Arc<dyn Backend>>,
    /// Per-row run state for recipe + script launches. Each entry
    /// owns its persistent output buffer; navigating the sidebar
    /// switches which buffer the right pane shows. A run remains in
    /// the map after completion so the user can scroll through its
    /// output until they explicitly relaunch (which replaces the
    /// entry).
    runs: BTreeMap<RunKey, RunState>,
    /// Most recent compose lifecycle action (`U` / `D` / `R` / `S`).
    /// Held separately because lifecycle actions don't have a
    /// per-row identity — they target the whole project. Output
    /// streams into the right pane via an overlay split when in
    /// flight; goes idle and stays inspectable when done.
    lifecycle_run: Option<RunState>,
    /// Service pane state, keyed by service name. Populated for every
    /// `[[ui.pane]] type = "service"` declaration in the config.
    services: BTreeMap<String, ServicePane>,
    /// Watcher pane state, keyed by synthesised name (`watch:<recipe>`
    /// suffixed with an index when collisions occur). Populated lazily
    /// in `spawn_watcher_panes`.
    watchers: BTreeMap<String, WatcherPane>,
    /// Last rejection / status banner (decays after a few seconds — kept
    /// simple by just clearing on the next successful action).
    pub flash: Option<String>,
    mode: Mode,
    palette: Option<Palette>,
    /// Open confirmation dialog, if any. Routes keys when
    /// `mode == Confirm`.
    confirm: Option<ConfirmDialog>,
    /// Open args prompt, if any. Routes keys when
    /// `mode == ArgsPrompt`.
    args_prompt: Option<ArgsPrompt>,
    /// Open worktree switcher, if any. Routes keys when
    /// `mode == WorktreeSwitcher`.
    switcher: Option<WorktreeSwitcher>,
    /// Path the event loop should hot-reload into. When set, `drive`
    /// returns `DriveOutcome::SwitchWorktree(path)` and the outer
    /// loop tears the App down and rebuilds it.
    pub pending_switch: Option<PathBuf>,
    /// Cached project root so the switcher can prefill its path
    /// input with the current parent dir.
    project_root: PathBuf,
    /// Active top-level view. Switched via the `T` / `g` / `c`
    /// global keybinds.
    view: View,
    /// Terminals view state (lazily initialised — empty Vec until
    /// the user first opens the view; the session name + tmux probe
    /// happen on first switch).
    terminals: TerminalsState,
    /// Pending attach request from the Terminals view. The event
    /// loop drains this between ticks: drops the events reader,
    /// leaves alternate screen, runs tmux attach, re-enters.
    pub pending_attach: Option<AttachRequest>,
    /// Diff view state. Lazily populated on first switch / refresh.
    diff: DiffState,
}

impl App {
    pub fn new(config: Arc<Config>) -> Self {
        let items = build_items(&config);
        let services = collect_service_panes(&config);
        let terminals = TerminalsState::default_for(&config);
        Self {
            config,
            items,
            selected: 0,
            quit: false,
            executor: None,
            backend: None,
            runs: BTreeMap::new(),
            lifecycle_run: None,
            services,
            watchers: BTreeMap::new(),
            flash: None,
            mode: Mode::Normal,
            palette: None,
            confirm: None,
            args_prompt: None,
            switcher: None,
            pending_switch: None,
            project_root: PathBuf::from("."),
            view: View::ControlCenter,
            terminals,
            pending_attach: None,
            diff: DiffState::default(),
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn switch_view(&mut self, view: View) {
        self.view = view;
    }

    /// Set the project root the App is bound to. Required for the
    /// worktree switcher to know where to enumerate worktrees from
    /// and what parent directory to prefill the new-form's path with.
    pub fn with_project_root(mut self, project_root: &Path) -> Self {
        self.project_root = project_root.to_path_buf();
        self
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn with_executor(mut self, executor: Executor) -> Self {
        self.executor = Some(executor);
        self
    }

    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn backend(&self) -> Option<&Arc<dyn Backend>> {
        self.backend.as_ref()
    }

    pub fn services(&self) -> &BTreeMap<String, ServicePane> {
        &self.services
    }

    pub fn services_mut(&mut self) -> &mut BTreeMap<String, ServicePane> {
        &mut self.services
    }

    pub fn selected_service(&self) -> Option<&ServicePane> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Service {
            return None;
        }
        self.services.get(&item.name)
    }

    pub fn watchers(&self) -> &BTreeMap<String, WatcherPane> {
        &self.watchers
    }

    pub fn selected_watcher(&self) -> Option<&WatcherPane> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Watcher {
            return None;
        }
        self.watchers.get(&item.name)
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn quit(&mut self) {
        self.quit = true;
    }

    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.items.len() - 1);
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        if !self.items.is_empty() {
            self.selected = self.items.len() - 1;
        }
    }

    pub fn selected_recipe(&self) -> Option<&Recipe> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Recipe {
            return None;
        }
        self.config.commands.get(&item.name)
    }

    pub fn selected_script(&self) -> Option<&ScriptCommand> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Script {
            return None;
        }
        self.config.scripts.get(&item.name)
    }

    /// Run state for a specific recipe / script row, if one exists.
    /// Returns `None` for services and watchers (which carry their
    /// own pane state) and for commands that have never been run.
    pub fn run_for(&self, kind: ItemKind, name: &str) -> Option<&RunState> {
        self.runs.get(&(kind, name.to_string()))
    }

    /// Run state for the currently-selected sidebar row, if any.
    /// Used by the renderer to paint the output pane.
    pub fn selected_run(&self) -> Option<&RunState> {
        let item = self.selected_item()?;
        self.run_for(item.kind, &item.name)
    }

    /// The compose-lifecycle run, if a `U` / `D` / `R` / `S` action
    /// has been triggered. Output streams here regardless of which
    /// sidebar row is selected — lifecycle actions are project-wide
    /// and don't have a row of their own.
    pub fn lifecycle_run(&self) -> Option<&RunState> {
        self.lifecycle_run.as_ref()
    }

    /// Try to launch the currently selected item with no extra args.
    /// Thin wrapper over [`Self::try_launch_selected_with_args`]; kept
    /// for callers that don't need to pass anything.
    pub fn try_launch_selected(&mut self) -> Result<(), LaunchRejection> {
        self.try_launch_selected_with_args(Vec::new())
    }

    /// Launch the currently selected item, forwarding `args` to it.
    /// Args reach the recipe / script via the same `forward_args =
    /// true` mechanism the CLI uses — the engine appends them to the
    /// argv after `shell_words` parses the run string.
    ///
    /// Returns [`LaunchRejection`] when the launch can't proceed.
    /// [`LaunchRejection::AlreadyRunning`] is the signal to open the
    /// kill-and-restart confirmation modal.
    pub fn try_launch_selected_with_args(
        &mut self,
        args: Vec<String>,
    ) -> Result<(), LaunchRejection> {
        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();

        // Per-row "is this already running?" gate replaces the old
        // single-slot `current_run` check. Different rows can run
        // concurrently; only the same row collides with itself.
        let key: RunKey = (item.kind, item.name.clone());
        if self.runs.get(&key).is_some_and(|r| !r.is_done()) {
            return Err(LaunchRejection::AlreadyRunning);
        }

        let executor = self
            .executor
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();

        match item.kind {
            ItemKind::Container => {
                return Err(LaunchRejection::NotRunnable(
                    "container row shows lifecycle output; use U / D / R / S".into(),
                ));
            }
            ItemKind::Service => {
                return Err(LaunchRejection::NotRunnable(
                    "service panes show logs; press enter to up the service".into(),
                ));
            }
            ItemKind::Watcher => {
                return Err(LaunchRejection::NotRunnable(
                    "watcher panes auto-run on file change".into(),
                ));
            }
            ItemKind::Recipe => {
                let Some(recipe) = self.config.commands.get(&item.name) else {
                    return Err(LaunchRejection::NotRunnable(format!(
                        "recipe `{}` vanished from config",
                        item.name
                    )));
                };
                if recipe.service.is_some() {
                    return Err(LaunchRejection::NotRunnable(
                        "in-container recipes from the TUI are deferred — run from the CLI".into(),
                    ));
                }
            }
            ItemKind::Script => {
                let Some(script) = self.config.scripts.get(&item.name) else {
                    return Err(LaunchRejection::NotRunnable(format!(
                        "script `{}` vanished from config",
                        item.name
                    )));
                };
                if script.service.is_some() {
                    return Err(LaunchRejection::NotRunnable(
                        "in-container scripts are deferred".into(),
                    ));
                }
            }
        }

        let run = RunState::spawn(&executor, item.name, args);
        self.runs.insert(key, run);
        self.flash = None;
        Ok(())
    }

    /// Force-relaunch the selected row even if a previous run is in
    /// flight: aborts the existing one, drops its buffer, and spawns
    /// a fresh `RunState`. Called by the kill-and-restart confirmation
    /// path after the user says yes.
    pub fn force_relaunch_selected(&mut self) -> Result<(), LaunchRejection> {
        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();
        let key: RunKey = (item.kind, item.name.clone());
        if let Some(run) = self.runs.get_mut(&key)
            && !run.is_done()
        {
            run.abort();
        }
        // Drop the old entry so the buffer starts fresh; try_launch_selected
        // would skip the spawn otherwise.
        self.runs.remove(&key);
        self.try_launch_selected()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn palette(&self) -> Option<&Palette> {
        self.palette.as_ref()
    }

    pub fn open_palette(&mut self) {
        self.mode = Mode::Palette;
        self.palette = Some(Palette::new(&self.items));
    }

    pub fn close_palette(&mut self) {
        self.mode = Mode::Normal;
        self.palette = None;
    }

    pub fn palette_mut(&mut self) -> Option<&mut Palette> {
        self.palette.as_mut()
    }

    /// Resolve the palette: select the matched row, close the palette,
    /// and launch the row with any args parsed from the palette input.
    /// `:echo-args foo bar` lands as a launch of `echo-args` with
    /// `["foo", "bar"]`. Returns the launch outcome:
    ///   - `None` when there was no match to confirm
    ///   - `Some(Ok(()))` on a clean launch
    ///   - `Some(Err(rej))` on a rejection the caller should flash
    ///
    /// `LaunchRejection::AlreadyRunning` is returned as-is; the caller
    /// is expected to open the kill-and-restart modal.
    pub fn confirm_palette(&mut self) -> Option<Result<(), LaunchRejection>> {
        let palette = self.palette.as_ref()?;
        let m = palette.selected_match()?;
        let args = palette.parsed_args();
        let item_idx = m.item_index;
        self.close_palette();
        self.selected = item_idx;

        // Services have their own action wiring (Enter = up). The
        // palette excludes services from candidates, but we re-check
        // here as a safety net so a config edit during the palette's
        // lifetime can't crash the launch path.
        let item = self.items.get(item_idx).cloned()?;
        if matches!(item.kind, ItemKind::Service | ItemKind::Watcher) {
            let kind = match item.kind {
                ItemKind::Service => "service",
                ItemKind::Watcher => "watcher",
                _ => unreachable!(),
            };
            return Some(Err(LaunchRejection::NotRunnable(format!(
                "{kind} `{}` can't be launched from the palette",
                item.name
            ))));
        }

        Some(self.try_launch_selected_with_args(args))
    }

    /// Open a kill-and-restart confirmation for the selected row. Sets
    /// `mode = Confirm` so the keymap routes to the modal handler. Body
    /// text references the run by name so users see *which* row they're
    /// interrupting (matters when the same key was pressed elsewhere).
    pub fn open_kill_restart_confirm(&mut self) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        let key: RunKey = (item.kind, item.name.clone());
        // No-op if the row isn't actually running; saves a confused
        // dialog for users with a stale keypress.
        if self.runs.get(&key).is_none_or(|r| r.is_done()) {
            return;
        }
        self.confirm = Some(ConfirmDialog {
            title: format!("`{}` is running", item.name),
            body: "Kill and restart?".into(),
            yes_focused: true,
            action: ConfirmAction::KillAndRestart { key },
        });
        self.mode = Mode::Confirm;
    }

    pub fn confirm_dialog(&self) -> Option<&ConfirmDialog> {
        self.confirm.as_ref()
    }

    pub fn confirm_toggle_focus(&mut self) {
        if let Some(c) = self.confirm.as_mut() {
            c.yes_focused = !c.yes_focused;
        }
    }

    /// Resolve the open confirmation, applying its action when the
    /// user accepted (`accept = true`) or simply dismissing otherwise.
    /// Returns the rejection (if any) from the resulting action so
    /// the caller can flash it.
    /// Whether the selected row is a `forward_args = true` recipe or
    /// script. Used by the keymap to decide whether Enter opens the
    /// args prompt or just launches.
    pub fn selected_accepts_args(&self) -> bool {
        let Some(item) = self.selected_item() else {
            return false;
        };
        match item.kind {
            ItemKind::Recipe => self
                .config
                .commands
                .get(&item.name)
                .is_some_and(|r| r.forward_args),
            ItemKind::Script => self
                .config
                .scripts
                .get(&item.name)
                .is_some_and(|s| s.forward_args),
            _ => false,
        }
    }

    /// Open the args prompt for the selected row. No-op if the row
    /// doesn't accept args (callers gate via `selected_accepts_args`).
    pub fn open_args_prompt(&mut self) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        if !self.selected_accepts_args() {
            return;
        }
        self.args_prompt = Some(ArgsPrompt {
            item_name: item.name,
            kind: item.kind,
            input: String::new(),
        });
        self.mode = Mode::ArgsPrompt;
    }

    pub fn args_prompt(&self) -> Option<&ArgsPrompt> {
        self.args_prompt.as_ref()
    }

    pub fn args_prompt_push_char(&mut self, c: char) {
        if let Some(p) = self.args_prompt.as_mut() {
            p.input.push(c);
        }
    }

    pub fn args_prompt_pop_char(&mut self) {
        if let Some(p) = self.args_prompt.as_mut() {
            p.input.pop();
        }
    }

    /// Resolve the args prompt: launch the row when `accept = true`,
    /// dismiss otherwise. Tokenises the input shell-style; returns
    /// the launch outcome (mirroring [`Self::confirm_palette`]).
    pub fn args_prompt_resolve(&mut self, accept: bool) -> Option<Result<(), LaunchRejection>> {
        let prompt = self.args_prompt.take()?;
        self.mode = Mode::Normal;
        if !accept {
            return None;
        }
        let args = if prompt.input.trim().is_empty() {
            Vec::new()
        } else {
            shell_words::split(&prompt.input).unwrap_or_default()
        };
        // Re-select in case the user navigated mid-prompt (key handler
        // shouldn't allow it, but defensive).
        if let Some(idx) = self
            .items
            .iter()
            .position(|i| i.kind == prompt.kind && i.name == prompt.item_name)
        {
            self.selected = idx;
        }
        Some(self.try_launch_selected_with_args(args))
    }

    /// Open the worktree switcher with `entries` (typically the
    /// output of `list_worktrees(project_root)`). The current row
    /// is auto-flagged so the user sees where they are.
    pub fn open_worktree_switcher(&mut self, entries: Vec<WorktreeRow>) {
        let selected = entries.iter().position(|e| e.is_current).unwrap_or(0);
        self.switcher = Some(WorktreeSwitcher {
            entries,
            selected,
            creating: None,
        });
        self.mode = Mode::WorktreeSwitcher;
    }

    pub fn switcher(&self) -> Option<&WorktreeSwitcher> {
        self.switcher.as_ref()
    }

    /// Close the switcher without acting.
    pub fn close_switcher(&mut self) {
        self.switcher = None;
        self.mode = Mode::Normal;
    }

    pub fn switcher_select_next(&mut self) {
        if let Some(s) = self.switcher.as_mut() {
            let total = s.total_rows();
            if total > 0 {
                s.selected = (s.selected + 1).min(total - 1);
            }
        }
    }

    pub fn switcher_select_prev(&mut self) {
        if let Some(s) = self.switcher.as_mut() {
            s.selected = s.selected.saturating_sub(1);
        }
    }

    /// Resolve the switcher: if the selected row is an existing
    /// worktree, queue a hot-reload to its path and close the modal;
    /// if it's the synthetic "+ new worktree" row, open the
    /// create-form sub-state instead. Returns true if the switcher
    /// was acted on (so the caller knows whether to absorb the key).
    pub fn switcher_confirm(&mut self) -> bool {
        let Some(s) = self.switcher.as_ref() else {
            return false;
        };
        if s.selected == s.new_row_index() {
            // Open the create form, prefilled.
            let parent = self
                .project_root
                .parent()
                .unwrap_or(self.project_root.as_path());
            let mut form = NewWorktreeForm {
                path_input: format!("{}/", parent.display()),
                branch_input: String::new(),
                focus: NewFormField::Branch,
                error: None,
            };
            // If we know the current branch, default the path to a
            // sibling dir named after a placeholder so users see
            // the shape and only need to type the new branch name.
            form.path_input = parent.join("").display().to_string();
            self.switcher.as_mut().unwrap().creating = Some(form);
            return true;
        }
        // Existing worktree row → queue switch.
        let row = s.entries[s.selected].clone();
        if !row.is_current {
            self.pending_switch = Some(row.path);
        }
        self.close_switcher();
        true
    }

    /// Mutate the open new-worktree form. Caller dispatches keys to
    /// these helpers from the switcher key handler.
    pub fn switcher_form_push_char(&mut self, c: char) {
        if let Some(form) = self.switcher.as_mut().and_then(|s| s.creating.as_mut()) {
            match form.focus {
                NewFormField::Path => form.path_input.push(c),
                NewFormField::Branch => form.branch_input.push(c),
            }
            form.error = None;
        }
    }

    pub fn switcher_form_pop_char(&mut self) {
        if let Some(form) = self.switcher.as_mut().and_then(|s| s.creating.as_mut()) {
            match form.focus {
                NewFormField::Path => {
                    form.path_input.pop();
                }
                NewFormField::Branch => {
                    form.branch_input.pop();
                }
            }
            form.error = None;
        }
    }

    pub fn switcher_form_toggle_focus(&mut self) {
        if let Some(form) = self.switcher.as_mut().and_then(|s| s.creating.as_mut()) {
            form.focus = match form.focus {
                NewFormField::Path => NewFormField::Branch,
                NewFormField::Branch => NewFormField::Path,
            };
        }
    }

    pub fn switcher_form_cancel(&mut self) {
        if let Some(s) = self.switcher.as_mut() {
            s.creating = None;
        }
    }

    /// Snapshot the current form state without taking ownership;
    /// the caller invokes git from this data and reports back via
    /// [`Self::switcher_form_finish`].
    pub fn switcher_form_snapshot(&self) -> Option<NewWorktreeForm> {
        self.switcher.as_ref().and_then(|s| s.creating.clone())
    }

    /// Resolve the form after a `git worktree add` attempt. On Ok,
    /// queues a switch to the freshly-created path and closes the
    /// modal. On Err, surfaces the message inside the form so the
    /// user can fix and retry.
    pub fn switcher_form_finish(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                self.pending_switch = Some(path);
                self.close_switcher();
            }
            Err(msg) => {
                if let Some(form) = self.switcher.as_mut().and_then(|s| s.creating.as_mut()) {
                    form.error = Some(msg);
                }
            }
        }
    }

    /// Take the queued worktree switch path, if any. Called by the
    /// event loop after `drive` returns; the path drives a full App
    /// rebuild against the new project root.
    pub fn take_pending_switch(&mut self) -> Option<PathBuf> {
        self.pending_switch.take()
    }

    pub fn terminals(&self) -> &TerminalsState {
        &self.terminals
    }

    pub fn terminals_mut(&mut self) -> &mut TerminalsState {
        &mut self.terminals
    }

    pub fn set_tmux_available(&mut self, available: bool) {
        self.terminals.tmux_available = Some(available);
    }

    /// Stable list of rows the Terminals view shows: every service
    /// (whether running or not — caller renders status indicators),
    /// then every custom terminal, then a sentinel "+ new" row.
    /// Indexes returned here align with `TerminalsState::selected`.
    pub fn terminals_rows(&self) -> Vec<TerminalsRow> {
        let mut rows = Vec::new();
        for name in self.services.keys() {
            rows.push(TerminalsRow::Service(name.clone()));
        }
        for t in &self.terminals.terminals {
            rows.push(TerminalsRow::Terminal(t.clone()));
        }
        rows.push(TerminalsRow::NewSentinel);
        rows
    }

    pub fn terminals_select_next(&mut self) {
        let total = self.terminals_rows().len();
        if total > 0 {
            self.terminals.selected = (self.terminals.selected + 1).min(total - 1);
        }
    }

    pub fn terminals_select_prev(&mut self) {
        self.terminals.selected = self.terminals.selected.saturating_sub(1);
    }

    /// Resolve the selected row in the Terminals view: queue an
    /// attach for service / custom rows; open the create form for
    /// the sentinel.
    pub fn terminals_confirm(&mut self) {
        let rows = self.terminals_rows();
        let Some(row) = rows.get(self.terminals.selected).cloned() else {
            return;
        };
        match row {
            TerminalsRow::Service(name) => {
                if let Err(msg) = self.queue_service_attach(&name) {
                    self.flash = Some(msg);
                }
            }
            TerminalsRow::Terminal(term) => {
                let session = self.terminals.session_name.clone();
                let window = term.name.clone();
                let create_with = match &term.kind {
                    TerminalKind::Shell => Some(format!(
                        "cd {} && exec ${{SHELL:-/bin/sh}}",
                        shell_escape(self.project_root.display().to_string())
                    )),
                    TerminalKind::Custom { command } => Some(command.clone()),
                    TerminalKind::ServiceShell { .. } => None, // unreachable — services come via Service variant
                };
                self.pending_attach = Some(AttachRequest {
                    session,
                    window,
                    create_with,
                });
            }
            TerminalsRow::NewSentinel => {
                self.terminals.creating = Some(NewTerminalForm {
                    name_input: String::new(),
                    command_input: String::new(),
                    focus: NewTerminalField::Name,
                    error: None,
                });
            }
        }
    }

    /// Delete the selected custom terminal. Services and the
    /// sentinel are not deletable; this no-ops on those.
    pub fn terminals_delete_selected(&mut self) {
        let rows = self.terminals_rows();
        let Some(row) = rows.get(self.terminals.selected) else {
            return;
        };
        let TerminalsRow::Terminal(target) = row else {
            return;
        };
        let target_name = target.name.clone();
        self.terminals.terminals.retain(|t| t.name != target_name);
        // Clamp selection so we don't point past the end after the
        // shrink.
        let new_total = self.terminals_rows().len();
        if self.terminals.selected >= new_total && new_total > 0 {
            self.terminals.selected = new_total - 1;
        }
    }

    pub fn terminals_form_push_char(&mut self, c: char) {
        if let Some(form) = self.terminals.creating.as_mut() {
            match form.focus {
                NewTerminalField::Name => form.name_input.push(c),
                NewTerminalField::Command => form.command_input.push(c),
            }
            form.error = None;
        }
    }

    pub fn terminals_form_pop_char(&mut self) {
        if let Some(form) = self.terminals.creating.as_mut() {
            match form.focus {
                NewTerminalField::Name => {
                    form.name_input.pop();
                }
                NewTerminalField::Command => {
                    form.command_input.pop();
                }
            }
            form.error = None;
        }
    }

    pub fn terminals_form_toggle_focus(&mut self) {
        if let Some(form) = self.terminals.creating.as_mut() {
            form.focus = match form.focus {
                NewTerminalField::Name => NewTerminalField::Command,
                NewTerminalField::Command => NewTerminalField::Name,
            };
        }
    }

    pub fn terminals_form_cancel(&mut self) {
        self.terminals.creating = None;
    }

    /// Save the form into a new TerminalRow. Empty name is rejected
    /// inline; empty command falls back to `Shell` in the project root.
    pub fn terminals_form_submit(&mut self) {
        let Some(form) = self.terminals.creating.as_mut() else {
            return;
        };
        let name = form.name_input.trim().to_string();
        if name.is_empty() {
            form.error = Some("name is required".into());
            return;
        }
        if self.terminals.terminals.iter().any(|t| t.name == name) {
            form.error = Some(format!("`{name}` already exists"));
            return;
        }
        let kind = if form.command_input.trim().is_empty() {
            TerminalKind::Shell
        } else {
            TerminalKind::Custom {
                command: form.command_input.trim().to_string(),
            }
        };
        self.terminals.terminals.push(TerminalRow { name, kind });
        self.terminals.creating = None;
    }

    pub fn take_pending_attach(&mut self) -> Option<AttachRequest> {
        self.pending_attach.take()
    }

    /// Queue an attach into the named compose service and jump to
    /// the Terminals view so the user lands there after `ctrl+b d`
    /// detaches. Used by both the control-center `enter`-on-service
    /// path and the Terminals-view `enter`-on-service-row path so
    /// the tmux invocation stays in one place.
    /// Queue an attach into the named compose service and jump to
    /// the Terminals view so the user lands there after `ctrl+b d`.
    /// Returns `Err(reason)` for non-container services (systemd,
    /// custom). Those don't have a shell to exec into — the caller
    /// flashes the reason rather than opening an empty pane.
    pub fn queue_service_attach(&mut self, service: &str) -> Result<(), String> {
        if self
            .config
            .services
            .systemd
            .iter()
            .any(|s| s.name == service)
        {
            return Err(format!(
                "`{service}` is a systemd unit — no interactive shell to attach to"
            ));
        }
        if self
            .config
            .services
            .custom
            .iter()
            .any(|s| s.name == service)
        {
            return Err(format!(
                "`{service}` is a custom service — create a custom terminal instead"
            ));
        }
        let session = self.terminals.session_name.clone();
        let window = format!("svc:{service}");
        let create_with = Some(format!(
            "docker compose exec -it {service} sh -c 'exec ${{SHELL:-/bin/sh}}'"
        ));
        self.pending_attach = Some(AttachRequest {
            session,
            window,
            create_with,
        });
        self.view = View::Terminals;
        Ok(())
    }

    pub fn diff(&self) -> &DiffState {
        &self.diff
    }

    /// Replace the file list (typically after a `git status` reload).
    /// Clamps the selection so it can't point past the end. Cache
    /// stays — the user might re-edit and want the same diff back.
    pub fn diff_set_files(&mut self, files: Vec<DiffFile>) {
        self.diff.files = files;
        self.diff.loaded = true;
        if self.diff.selected >= self.diff.files.len() {
            self.diff.selected = self.diff.files.len().saturating_sub(1);
        }
        self.diff.error = None;
    }

    pub fn diff_set_error(&mut self, msg: String) {
        self.diff.error = Some(msg);
        self.diff.loaded = true;
    }

    pub fn diff_select_next(&mut self) {
        if self.diff.files.is_empty() {
            return;
        }
        self.diff.selected = (self.diff.selected + 1).min(self.diff.files.len() - 1);
    }

    pub fn diff_select_prev(&mut self) {
        self.diff.selected = self.diff.selected.saturating_sub(1);
    }

    pub fn diff_selected_file(&self) -> Option<&DiffFile> {
        self.diff.files.get(self.diff.selected)
    }

    pub fn diff_cache_for(&self, path: &str) -> Option<&Vec<DiffLine>> {
        self.diff.cache.get(path)
    }

    pub fn diff_set_cache(&mut self, path: String, lines: Vec<DiffLine>) {
        self.diff.cache.insert(path, lines);
    }

    /// Mark the diff state stale so the next render pulls fresh
    /// data. Used by the `r` keybind in the diff view.
    pub fn diff_mark_stale(&mut self) {
        self.diff.loaded = false;
        self.diff.cache.clear();
    }

    pub fn confirm_resolve(&mut self, accept: bool) -> Option<LaunchRejection> {
        let d = self.confirm.take()?;
        self.mode = Mode::Normal;
        if !accept {
            return None;
        }
        match d.action {
            ConfirmAction::KillAndRestart { key } => {
                // Re-select the row the dialog was opened for, so
                // force_relaunch_selected operates on the right one
                // even if the user navigated mid-dialog.
                if let Some(idx) = self
                    .items
                    .iter()
                    .position(|i| i.kind == key.0 && i.name == key.1)
                {
                    self.selected = idx;
                }
                self.force_relaunch_selected().err()
            }
        }
    }

    /// Drain output for every active run plus the lifecycle slot.
    /// Cheap when nothing has arrived; called on every pre-render
    /// hook so navigation reveals up-to-date buffers.
    pub fn drain_runs(&mut self) {
        for run in self.runs.values_mut() {
            run.drain();
        }
        if let Some(run) = self.lifecycle_run.as_mut() {
            run.drain();
        }
    }

    /// Abort the run on the selected row, if it's in flight. Returns
    /// true when an abort was issued. With per-row runs, `s` no
    /// longer needs to know "is *anything* running" — only "is the
    /// thing I'm focused on running."
    pub fn abort_selected_run(&mut self) -> bool {
        let Some(item) = self.selected_item().cloned() else {
            return false;
        };
        let key: RunKey = (item.kind, item.name);
        match self.runs.get_mut(&key) {
            Some(run) if !run.is_done() => {
                run.abort();
                true
            }
            _ => false,
        }
    }

    /// Abort the lifecycle run if one is in flight. Used by `s` /
    /// `S` when no per-row run is active — keeps the "stop the
    /// noisy thing" muscle memory working.
    pub fn abort_lifecycle_run(&mut self) -> bool {
        match self.lifecycle_run.as_mut() {
            Some(run) if !run.is_done() => {
                run.abort();
                true
            }
            _ => false,
        }
    }

    /// Run a backend lifecycle action (up / down / stop / restart)
    /// against the given services. An empty slice means "all services".
    /// Output streams into the lifecycle slot independent of which
    /// sidebar row is selected; the renderer overlays it as a split
    /// view when in flight.
    ///
    /// Returns `Err` if no backend is configured or another lifecycle
    /// action is already running. Per-row recipe / script runs do not
    /// block lifecycle actions.
    pub async fn run_service_action(
        &mut self,
        action: &'static str,
        services: &[&str],
    ) -> Result<(), LaunchRejection> {
        if self.lifecycle_run.as_ref().is_some_and(|r| !r.is_done()) {
            return Err(LaunchRejection::AlreadyRunning);
        }
        let backend = self
            .backend
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();
        match backend.service_action(action, services).await {
            Ok(child) => {
                let label = if services.is_empty() {
                    format!("compose {action}")
                } else {
                    format!("compose {action} {}", services.join(" "))
                };
                self.lifecycle_run = Some(RunState::spawn_child(label, child));
                // Auto-jump to the container row so the user sees the
                // action's output immediately. They can navigate away
                // mid-stream; the buffer stays in the row until the
                // next lifecycle action replaces it.
                if let Some(idx) = self
                    .items
                    .iter()
                    .position(|i| i.kind == ItemKind::Container)
                {
                    self.selected = idx;
                }
                Ok(())
            }
            Err(e) => Err(LaunchRejection::NotRunnable(format!("{e}"))),
        }
    }

    /// Poll completion for every active run plus the lifecycle slot.
    /// Each `RunState` short-circuits on already-done.
    pub async fn poll_runs(&mut self) {
        for run in self.runs.values_mut() {
            run.poll_completion().await;
        }
        if let Some(run) = self.lifecycle_run.as_mut() {
            run.poll_completion().await;
        }
    }

    /// Drain output from every service pane and detect any tail
    /// process that exited non-zero (so the error message gets
    /// surfaced on the padded error path). Cheap when nothing has
    /// arrived; called on every pre-render hook.
    pub fn drain_services(&mut self) {
        for pane in self.services.values_mut() {
            pane.poll_tail();
        }
    }

    /// Spawn tail processes for every service pane that doesn't already
    /// have one. Called once at startup; idempotent if called again.
    pub async fn spawn_service_tails(&mut self) {
        let Some(backend) = self.backend.as_ref().map(Arc::clone) else {
            return;
        };
        for pane in self.services.values_mut() {
            pane.ensure_tailing(&backend).await;
        }
    }

    /// Auto-discover services from the backend and add ones not already
    /// declared in `[[ui.pane]]`. Idempotent. Called once at startup,
    /// before [`Self::spawn_service_tails`].
    pub async fn discover_services(&mut self) {
        let Some(backend) = self.backend.as_ref().map(Arc::clone) else {
            return;
        };
        let Ok(discovered) = backend.list_services().await else {
            return;
        };
        let mut added = false;
        for name in discovered {
            if self.services.contains_key(&name) {
                continue;
            }
            self.services
                .insert(name.clone(), ServicePane::new(name.clone()));
            added = true;
        }
        if added {
            // Rebuild items so the new services land in the sidebar's
            // services group, preserving ordering for the rest.
            self.items = build_items_from(&self.config, &self.services, &self.watchers);
        }
    }

    /// Refresh service status indicators. Each pane decides whether
    /// enough time has elapsed to actually re-poll.
    pub async fn refresh_service_status(&mut self) {
        let Some(backend) = self.backend.as_ref().map(Arc::clone) else {
            return;
        };
        for pane in self.services.values_mut() {
            pane.refresh_status(&backend).await;
        }
    }

    /// Spawn watcher panes from `[[ui.pane]] type = "watcher"`. Spawn
    /// failures (bad globs, notify init failures) are logged and the
    /// pane is omitted; the rest of the dashboard still works.
    pub fn spawn_watcher_panes(&mut self, project_root: &Path) {
        let mut added = false;
        for (idx, pane) in self.config.ui.panes.iter().enumerate() {
            if let UiPane::Watcher {
                glob,
                on_change,
                debounce_ms,
                ..
            } = pane
            {
                let name = unique_watcher_name(on_change, idx, &self.watchers);
                match WatcherPane::spawn(
                    name.clone(),
                    on_change.clone(),
                    glob.clone(),
                    Duration::from_millis(*debounce_ms),
                    project_root,
                ) {
                    Ok(p) => {
                        self.watchers.insert(name, p);
                        added = true;
                    }
                    Err(e) => {
                        tracing::warn!("watcher pane `{on_change}` failed to start: {e}");
                    }
                }
            }
        }
        if added {
            self.items = build_items_from(&self.config, &self.services, &self.watchers);
        }
    }

    /// Advance every watcher's state machine. Spawn / drain / complete is
    /// internal to the pane.
    pub async fn tick_watchers(&mut self) {
        let Some(executor) = self.executor.clone() else {
            return;
        };
        for pane in self.watchers.values_mut() {
            pane.tick(&executor).await;
        }
    }
}

fn unique_watcher_name(base: &str, idx: usize, existing: &BTreeMap<String, WatcherPane>) -> String {
    let primary = format!("watch:{base}");
    if !existing.contains_key(&primary) {
        return primary;
    }
    format!("watch:{base}:{idx}")
}

fn build_items(config: &Config) -> Vec<Item> {
    build_items_from(config, &collect_service_panes(config), &BTreeMap::new())
}

/// Returns the row label for the container backend, or `None` when
/// no container backend is configured (`backend = "none"`). The row
/// label is the backend name the user typed in `[containers]
/// backend = "..."` so the sidebar mirrors their config.
fn container_row_label(config: &Config) -> Option<&'static str> {
    use scaffl_config::model::Backend as B;
    match config.containers.backend {
        B::None => None,
        B::Compose => Some("compose"),
        B::Docker => Some("docker"),
        B::Podman => Some("podman"),
    }
}

/// Reconstruct the sidebar item list from live state. The order is
/// stable: container (when configured), services (declared first in
/// scaffl.toml order, then any auto-discovered ones), watchers,
/// recipes, scripts.
fn build_items_from(
    config: &Config,
    services: &BTreeMap<String, ServicePane>,
    watchers: &BTreeMap<String, WatcherPane>,
) -> Vec<Item> {
    let mut items = Vec::new();

    // Container row first (when a backend is configured) — this is
    // the canonical home for compose lifecycle output (`U` / `D` /
    // `R` / `S`). One row, fixed name, top of the sidebar.
    if let Some(name) = container_row_label(config) {
        items.push(Item {
            name: name.to_string(),
            kind: ItemKind::Container,
        });
    }

    let mut emitted_services: std::collections::BTreeSet<&str> = Default::default();

    // Declared services first, in scaffl.toml [[ui.pane]] order.
    for pane in &config.ui.panes {
        if let UiPane::Service { service, .. } = pane
            && services.contains_key(service)
            && emitted_services.insert(service.as_str())
        {
            items.push(Item {
                name: service.clone(),
                kind: ItemKind::Service,
            });
        }
    }
    // Auto-discovered services follow (alphabetical via BTreeMap iter).
    for name in services.keys() {
        if emitted_services.insert(name.as_str()) {
            items.push(Item {
                name: name.clone(),
                kind: ItemKind::Service,
            });
        }
    }
    // Watchers (BTreeMap iteration is stable / alphabetical).
    for name in watchers.keys() {
        items.push(Item {
            name: name.clone(),
            kind: ItemKind::Watcher,
        });
    }
    items.extend(config.commands.keys().map(|name| Item {
        name: name.clone(),
        kind: ItemKind::Recipe,
    }));
    items.extend(config.scripts.keys().map(|name| Item {
        name: name.clone(),
        kind: ItemKind::Script,
    }));
    items
}

fn collect_service_panes(config: &Config) -> BTreeMap<String, ServicePane> {
    let mut out = BTreeMap::new();
    for pane in &config.ui.panes {
        if let UiPane::Service { service, .. } = pane {
            out.entry(service.clone())
                .or_insert_with(|| ServicePane::new(service.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Test cfg with no container backend, so the synthetic container
    /// row doesn't pollute item-count / index assertions for tests
    /// whose subject is unrelated.
    fn cfg() -> Arc<Config> {
        Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [command.up]
                run = "true"
                [command.test]
                run = "true"
                desc = "Run tests"
            "#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn build_items_orders_services_recipes_scripts() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [command.up]
                run = "true"

                [[ui.pane]]
                type = "service"
                service = "app"
            "#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert_eq!(app.items().len(), 2);
        assert_eq!(app.items()[0].kind, ItemKind::Service);
        assert_eq!(app.items()[0].name, "app");
        assert_eq!(app.items()[1].kind, ItemKind::Recipe);
    }

    #[test]
    fn container_row_appears_when_backend_configured() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "compose"

                [command.up]
                run = "true"
            "#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert_eq!(app.items()[0].kind, ItemKind::Container);
        assert_eq!(app.items()[0].name, "compose");
    }

    #[test]
    fn no_container_row_when_backend_none() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"
                [command.up]
                run = "true"
            "#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert!(app.items().iter().all(|i| i.kind != ItemKind::Container));
    }

    #[test]
    fn collect_service_panes_picks_up_ui_services() {
        let cfg = scaffl_config::parse_str(
            r#"
            [[ui.pane]]
            type = "service"
            service = "app"

            [[ui.pane]]
            type = "service"
            service = "worker"
            "#,
        )
        .unwrap();
        let panes = collect_service_panes(&cfg);
        assert_eq!(panes.len(), 2);
        assert!(panes.contains_key("app"));
        assert!(panes.contains_key("worker"));
    }

    #[test]
    fn launch_rejects_service_selection() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [[ui.pane]]
                type = "service"
                service = "app"
            "#,
            )
            .unwrap(),
        );
        let backend: Arc<dyn scaffl_container::Backend> =
            Arc::new(scaffl_container::null::NullBackend);
        let executor = Executor::new(backend, Arc::clone(&cfg), std::path::Path::new("/tmp"));
        let mut app = App::new(cfg).with_executor(executor);
        let err = app.try_launch_selected().unwrap_err();
        match err {
            LaunchRejection::NotRunnable(msg) => assert!(msg.contains("service panes")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn navigation_clamps_at_bounds() {
        let mut app = App::new(cfg());
        app.select_prev();
        assert_eq!(app.selected_index(), 0);
        app.select_next();
        assert_eq!(app.selected_index(), 1);
        app.select_next();
        assert_eq!(app.selected_index(), 1);
        app.select_first();
        assert_eq!(app.selected_index(), 0);
        app.select_last();
        assert_eq!(app.selected_index(), 1);
    }

    #[test]
    fn quit_flag_propagates() {
        let mut app = App::new(cfg());
        assert!(!app.should_quit());
        app.quit();
        assert!(app.should_quit());
    }

    #[test]
    fn truly_empty_config_has_no_items() {
        // Default Config has backend = compose, which produces a
        // container row. To exercise the genuinely-empty path we
        // explicitly disable the backend.
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"[containers]
                backend = "none""#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert_eq!(app.items().len(), 0);
        assert!(app.selected_item().is_none());
    }

    #[test]
    fn launch_without_executor_is_rejected() {
        let mut app = App::new(cfg());
        let err = app.try_launch_selected().unwrap_err();
        assert!(matches!(err, LaunchRejection::NoExecutor));
    }

    #[test]
    fn launch_rejects_in_container_recipe() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [command.shell]
                in = "app"
                run = "/bin/sh"
            "#,
            )
            .unwrap(),
        );
        let backend: Arc<dyn scaffl_container::Backend> =
            Arc::new(scaffl_container::null::NullBackend);
        let executor = Executor::new(backend, Arc::clone(&cfg), std::path::Path::new("/tmp"));
        let mut app = App::new(cfg).with_executor(executor);
        let err = app.try_launch_selected().unwrap_err();
        match err {
            LaunchRejection::NotRunnable(msg) => assert!(msg.contains("in-container")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn rows() -> Vec<WorktreeRow> {
        vec![
            WorktreeRow {
                path: PathBuf::from("/repo"),
                branch: Some("main".into()),
                slug: "main".into(),
                is_current: true,
            },
            WorktreeRow {
                path: PathBuf::from("/repo-feature"),
                branch: Some("feature".into()),
                slug: "feature".into(),
                is_current: false,
            },
        ]
    }

    #[test]
    fn switcher_open_preselects_current_row() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
        let s = app.switcher().unwrap();
        // Current row is index 0, so selected starts at 0.
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn switcher_confirm_on_current_row_no_pending_switch() {
        // Selecting the current worktree → no-op (no pending switch).
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        let acted = app.switcher_confirm();
        assert!(acted);
        assert!(app.pending_switch.is_none());
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn switcher_confirm_on_other_row_queues_switch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        let acted = app.switcher_confirm();
        assert!(acted);
        assert_eq!(
            app.take_pending_switch(),
            Some(PathBuf::from("/repo-feature"))
        );
    }

    #[test]
    fn switcher_navigation_extends_to_new_row_sentinel() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        // 2 entries + 1 sentinel = 3 rows. Down × 2 reaches the
        // sentinel; another Down clamps.
        app.switcher_select_next();
        app.switcher_select_next();
        assert_eq!(app.switcher().unwrap().selected, 2);
        app.switcher_select_next();
        assert_eq!(app.switcher().unwrap().selected, 2);
    }

    #[test]
    fn switcher_confirm_on_new_row_opens_form() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        // Modal stays open with the form populated.
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
        assert!(app.switcher().unwrap().creating.is_some());
    }

    #[test]
    fn switcher_form_finish_ok_queues_switch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.switcher_form_finish(Ok(PathBuf::from("/repo-new")));
        assert_eq!(app.take_pending_switch(), Some(PathBuf::from("/repo-new")));
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn switcher_form_finish_err_keeps_form_open_with_error() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.switcher_form_finish(Err("git: branch already exists".into()));
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.error.as_deref(), Some("git: branch already exists"));
    }

    #[test]
    fn terminals_default_has_one_shell_row() {
        let app = App::new(cfg());
        assert_eq!(app.terminals().terminals.len(), 1);
        assert_eq!(app.terminals().terminals[0].name, "shell");
    }

    #[test]
    fn terminals_form_submit_adds_row() {
        let mut app = App::new(cfg());
        // Open the form by jumping to the sentinel (last row).
        let total = app.terminals_rows().len();
        for _ in 0..total {
            app.terminals_select_next();
        }
        app.terminals_confirm();
        // Type "build" + cmd "make".
        for c in "build".chars() {
            app.terminals_form_push_char(c);
        }
        app.terminals_form_toggle_focus();
        for c in "make".chars() {
            app.terminals_form_push_char(c);
        }
        app.terminals_form_submit();
        assert!(app.terminals().creating.is_none());
        assert!(app.terminals().terminals.iter().any(|t| t.name == "build"));
    }

    #[test]
    fn terminals_form_rejects_empty_name() {
        let mut app = App::new(cfg());
        let total = app.terminals_rows().len();
        for _ in 0..total {
            app.terminals_select_next();
        }
        app.terminals_confirm();
        app.terminals_form_submit();
        let form = app.terminals().creating.as_ref().unwrap();
        assert_eq!(form.error.as_deref(), Some("name is required"));
    }

    #[test]
    fn terminals_form_rejects_duplicate_name() {
        let mut app = App::new(cfg());
        let total = app.terminals_rows().len();
        for _ in 0..total {
            app.terminals_select_next();
        }
        app.terminals_confirm();
        for c in "shell".chars() {
            app.terminals_form_push_char(c);
        }
        app.terminals_form_submit();
        let form = app.terminals().creating.as_ref().unwrap();
        assert!(form.error.as_deref().unwrap().contains("already exists"));
    }

    #[test]
    fn terminals_delete_removes_only_custom_rows() {
        let mut app = App::new(cfg());
        // Add a custom row first via the form.
        let total = app.terminals_rows().len();
        for _ in 0..total {
            app.terminals_select_next();
        }
        app.terminals_confirm();
        for c in "extra".chars() {
            app.terminals_form_push_char(c);
        }
        app.terminals_form_submit();
        // Now select the new row (it lands before sentinel).
        let rows = app.terminals_rows();
        let idx = rows
            .iter()
            .position(|r| matches!(r, crate::app::TerminalsRow::Terminal(t) if t.name == "extra"))
            .unwrap();
        app.terminals.selected = idx;
        app.terminals_delete_selected();
        assert!(!app.terminals().terminals.iter().any(|t| t.name == "extra"));
        // Default `shell` row stays.
        assert!(app.terminals().terminals.iter().any(|t| t.name == "shell"));
    }

    #[test]
    fn terminals_confirm_on_shell_row_queues_attach() {
        let mut app = App::new(cfg());
        // First terminal row in the default state is "shell" (no
        // services in this fixture).
        app.terminals.selected = 0;
        app.terminals_confirm();
        let req = app.take_pending_attach().unwrap();
        assert!(req.session.starts_with("scaffl-"));
        assert_eq!(req.window, "shell");
        assert!(req.create_with.as_deref().unwrap().contains("SHELL"));
    }

    #[test]
    fn diff_line_kind_classifies_known_prefixes() {
        use crate::app::DiffLineKind;
        assert_eq!(
            DiffLineKind::classify("@@ -1,5 +1,5 @@"),
            DiffLineKind::Hunk
        );
        assert_eq!(
            DiffLineKind::classify("diff --git a/x b/x"),
            DiffLineKind::Header
        );
        assert_eq!(DiffLineKind::classify("--- a/x"), DiffLineKind::Header);
        assert_eq!(DiffLineKind::classify("+++ b/x"), DiffLineKind::Header);
        assert_eq!(DiffLineKind::classify("+ added"), DiffLineKind::Added);
        assert_eq!(DiffLineKind::classify("- removed"), DiffLineKind::Removed);
        assert_eq!(DiffLineKind::classify(" context"), DiffLineKind::Context);
        assert_eq!(DiffLineKind::classify(""), DiffLineKind::Context);
    }

    #[test]
    fn queue_service_attach_jumps_view_and_sets_request() {
        let mut app = App::new(cfg());
        app.queue_service_attach("app").unwrap();
        assert_eq!(app.view(), View::Terminals);
        let req = app.take_pending_attach().unwrap();
        assert_eq!(req.window, "svc:app");
        assert!(
            req.create_with
                .as_deref()
                .unwrap()
                .contains("docker compose")
        );
    }

    #[test]
    fn queue_service_attach_rejects_systemd_service() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [[services.systemd]]
                name = "postgres"
                unit = "postgresql.service"
            "#,
            )
            .unwrap(),
        );
        let mut app = App::new(cfg);
        let err = app.queue_service_attach("postgres").unwrap_err();
        assert!(err.contains("systemd"), "msg: {err}");
        assert_eq!(app.view(), View::ControlCenter);
        assert!(app.take_pending_attach().is_none());
    }

    #[test]
    fn queue_service_attach_rejects_custom_service() {
        let cfg = Arc::new(
            scaffl_config::parse_str(
                r#"
                [containers]
                backend = "none"

                [[services.custom]]
                name   = "tunnel"
                status = "true"
                start  = "true"
                stop   = "true"
            "#,
            )
            .unwrap(),
        );
        let mut app = App::new(cfg);
        let err = app.queue_service_attach("tunnel").unwrap_err();
        assert!(err.contains("custom"), "msg: {err}");
    }

    #[test]
    fn diff_select_clamps_at_bounds() {
        let mut app = App::new(cfg());
        app.diff_set_files(vec![
            DiffFile {
                path: "a".into(),
                status: DiffStatus::Modified,
            },
            DiffFile {
                path: "b".into(),
                status: DiffStatus::Added,
            },
        ]);
        app.diff_select_next();
        assert_eq!(app.diff().selected, 1);
        app.diff_select_next();
        assert_eq!(app.diff().selected, 1);
        app.diff_select_prev();
        assert_eq!(app.diff().selected, 0);
        app.diff_select_prev();
        assert_eq!(app.diff().selected, 0);
    }

    #[test]
    fn diff_mark_stale_clears_cache() {
        let mut app = App::new(cfg());
        app.diff_set_files(vec![DiffFile {
            path: "a".into(),
            status: DiffStatus::Modified,
        }]);
        app.diff_set_cache("a".into(), vec![]);
        assert!(app.diff_cache_for("a").is_some());
        app.diff_mark_stale();
        assert!(app.diff_cache_for("a").is_none());
        assert!(!app.diff().loaded);
    }

    #[test]
    fn switcher_form_focus_toggles_between_path_and_branch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        let initial = app.switcher().unwrap().creating.as_ref().unwrap().focus;
        app.switcher_form_toggle_focus();
        let toggled = app.switcher().unwrap().creating.as_ref().unwrap().focus;
        assert_ne!(initial, toggled);
    }
}
