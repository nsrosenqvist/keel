//! TUI application state.
//!
//! The model and the controller. Pure functions — no terminal I/O here.

use crate::config::{Config, Recipe, ScriptCommand, model::UiPane};
use crate::container::Backend;
use crate::container::devcontainer::DevcontainerBackend;
use crate::runtime::Executor;
use crate::tui::dialogs::Modal;
use crate::tui::dialogs::args_prompt::ArgsPrompt;
use crate::tui::dialogs::confirm::{ConfirmAction, ConfirmDialog};
use crate::tui::dialogs::switcher::{
    BranchSpec, NewFormField, NewWorktreeAction, NewWorktreeForm, SwitcherConfirm, WorktreeRow,
    WorktreeSwitcher,
};
use crate::tui::palette::Palette;
use crate::tui::runner::RunState;
use crate::tui::services::ServicePane;
use crate::tui::views::control_center::state::{Item, ItemKind};
use crate::tui::views::diff::state::{BodyMode, DiffFile, DiffLine, DiffView, ReadLine};
use crate::tui::views::terminals::state::{TerminalsRow, TerminalsView, TmuxWindow};
use crate::tui::watchers::{WatcherError, WatcherPane};
use ratatui::layout::Rect;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

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

/// What a mouse click landed on. Lets the event handler detect a
/// repeat click on the *same* row (double-click → activate) versus a
/// click on a different row (just select).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    /// Control-center sidebar item, indexed into `app.items()`.
    SidebarItem(usize),
    /// Terminals view row, indexed into `app.terminals_rows()`.
    TerminalsRow(usize),
    /// Diff view file-list entry, indexed into `app.diff().files`.
    DiffFile(usize),
    /// Palette match row, indexed into `palette.matches()`.
    PaletteRow(usize),
    /// Worktree switcher row, indexed into `switcher.entries`. The
    /// final entry is the synthetic "+ new worktree" sentinel.
    SwitcherRow(usize),
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

/// One-shot signal from the Terminals view to the event loop:
/// "leave the alternate screen, attach to this tmux window, come
/// back when the user detaches." Created (or, if needed, sent
/// alongside a window-creation command) when the user hits enter
/// on a terminal row.
#[derive(Debug, Clone)]
pub struct AttachRequest {
    pub session: String,
    /// Either a window name (existing) or a marker like `keel-new`
    /// telling the attach handler to spawn a fresh window with
    /// `create_with`.
    pub window: String,
    /// When set, the attach handler runs this shell command first
    /// to create the window. `None` means the window already exists.
    pub create_with: Option<String>,
    /// When set, the event loop awaits the devcontainer's `ensure_up`
    /// before handing the terminal over to tmux. The attach is
    /// aborted (with a flashed error) if ensure-up fails — better to
    /// flash than to drop the user into a tmux window that immediately
    /// dies because `docker exec` couldn't find the container.
    pub ensure: Option<EnsureDevcontainer>,
    /// Explicit `-n <name>` passed to `tmux new-window` when the
    /// attach handler creates the window. Locks the name against
    /// tmux's automatic-rename (which would otherwise rewrite it to
    /// the foreground process — "docker" for devcontainer shells).
    /// `None` keeps the historical behaviour (let tmux pick).
    pub window_name: Option<String>,
    /// User-defined tmux window options to set after the window is
    /// created — surfaces as `#{@key}` in tmux formats. keel uses
    /// this to tag devcontainer windows with their workspace folder
    /// so the terminals sidebar can show the in-container path.
    pub window_options: Vec<(String, String)>,
}

/// Tiny carrier struct that lets `queue_new_shell` stay synchronous
/// while the actual `docker` work happens in the event loop's async
/// turn. Holding the Arc here is what makes the borrowing line up —
/// the App can produce the request, drop the immutable borrow, and
/// the event loop can then await on the backend.
#[derive(Clone)]
pub struct EnsureDevcontainer {
    pub backend: Arc<DevcontainerBackend>,
}

impl std::fmt::Debug for EnsureDevcontainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnsureDevcontainer")
            .field("container", &self.backend.container_name())
            .finish()
    }
}

/// Pending request to kill a tmux window. Drained by the event
/// loop before re-rendering, then the windows list refreshes.
#[derive(Debug, Clone)]
pub struct KillWindow {
    pub session: String,
    pub index: u32,
}

/// A queued one-shot side effect for the event loop to act on
/// between ticks. Handlers push these via [`App::queue`]; the loop
/// drains them via [`App::drain_commands`] and dispatches each by
/// variant. Replaces the four parallel `pending_*` fields the App
/// used to carry — adding a new async side effect is now one
/// variant + one match arm instead of one field + one accessor +
/// one drain check.
#[derive(Debug)]
pub enum Command {
    /// Hot-reload into another worktree. The event loop returns
    /// `DriveOutcome::SwitchWorktree` and the outer CLI loop tears
    /// down + rebuilds the App against the new project root.
    SwitchWorktree(PathBuf),
    /// Drop into a tmux session/window. The event loop leaves the
    /// alternate screen, runs `tmux attach`, and re-enters when the
    /// user detaches.
    AttachTmux(AttachRequest),
    /// Run `tmux kill-window` against `(session, index)`. The
    /// terminals view refreshes its window list afterwards.
    KillTmuxWindow(KillWindow),
    /// Suspend the TUI, run `lazygit` foreground, and resume when
    /// the user q's out.
    OpenLazygit,
    /// Write `\x07` to stdout so the outer terminal emulator
    /// triggers its configured bell action (audible beep, OS
    /// notification, dock badge). Edge-armed by
    /// [`crate::tui::views::terminals::state::TerminalsView::set_windows`]
    /// when a watched window's `has_bell` flips false→true.
    EmitBell,
}

/// Inbound message delivered from a background tokio task back to
/// the App's render loop. Phase 10 of the architectural refactor:
/// the App used to expose `async fn`s that held `&mut self` across
/// `.await` points, blocking the render loop on slow I/O (tmux
/// probe, diff preload, …). The conversion is: spawn the async work
/// on its own task, post a `Message` back here, and have a
/// synchronous handler on App apply it during the next render-loop
/// drain. Handlers stay pure (`(state, event) -> state'`); the loop
/// never sees a `&mut App` borrow held across an await.
#[derive(Debug)]
pub enum Message {
    /// `tmux -V` probe result — `true` when tmux is on PATH, `false`
    /// when missing or the probe failed. Posted by
    /// [`App::request_tmux_probe`]; applied by
    /// [`crate::tui::views::terminals::state::TerminalsView::set_tmux_available`].
    TmuxProbeResult(bool),
    /// Result of a top-level diff reload (anchor refresh + file
    /// list). Posted by [`App::request_diff_reload`]; applied by
    /// folding into [`crate::tui::views::diff::state::DiffView`]
    /// and kicking off the per-file body load for the selected file.
    DiffReloaded(DiffPreload),
    /// Result of a per-file diff body load. Posted by
    /// [`App::request_diff_for_selected`].
    DiffFileLoaded {
        path: String,
        lines: Vec<DiffLine>,
    },
    /// Result of a per-file read body load. Posted by
    /// [`App::request_read_for_selected`].
    ReadFileLoaded {
        path: String,
        lines: Vec<ReadLine>,
    },
}

/// Recompute `filtered` from the current `branch_input`. Empty
/// query → every branch in original order; non-empty → case-
/// insensitive substring match (good enough for v1; can swap in
/// nucleo-matcher later if anyone asks for fuzzy ordering).
fn refilter_branches(form: &mut NewWorktreeForm) {
    if form.branch_input.is_empty() {
        form.filtered = (0..form.branches.len()).collect();
    } else {
        let q = form.branch_input.to_lowercase();
        form.filtered = form
            .branches
            .iter()
            .enumerate()
            .filter(|(_, b)| b.name.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
    }
    // Selection clamps to the new bounds. If the user types until
    // the list shrinks to just the sentinel, that's selected.
    let total = form.total_options();
    form.selected = if total == 0 {
        0
    } else {
        form.selected.min(total - 1)
    };
}

/// Auto-fill the path field as `<parent>/<slug(branch)>` whenever
/// the user is typing in the branch field AND hasn't manually
/// edited the path yet. Once `path_dirty` is true, we leave the
/// path alone — the user owns it.
fn sync_path_from_branch(form: &mut NewWorktreeForm) {
    if form.path_dirty {
        return;
    }
    let dir = if form.branch_input.is_empty() {
        String::new()
    } else {
        crate::runtime::slugify(&form.branch_input)
    };
    if dir.is_empty() {
        form.path_input.clear();
    } else {
        form.path_input = form.parent.join(dir).display().to_string();
    }
}


/// Resolved diff state from a background load: top-level reload
/// triggered by `r` (or first-entry into the diff view) and the
/// boot-task preload both produce one of these. Folded into
/// [`DiffView`] by [`App::apply_diff_preload`].
#[derive(Debug)]
pub struct DiffPreload {
    pub trunk: Option<String>,
    pub anchor: Option<String>,
    pub anchor_short: Option<String>,
    pub branch: Option<String>,
    pub lazygit_available: bool,
    pub files: Result<Vec<DiffFile>, String>,
}

/// One pane's worth of watcher-spawn result, delivered from the boot
/// task. Spawns happen on a `spawn_blocking` thread because
/// [`WatcherPane::spawn`] is sync; results stream back so a slow
/// pane (notify init on a deep tree) doesn't hold up the rest.
pub struct WatcherSpawnResult {
    pub name: String,
    pub pane: Result<WatcherPane, WatcherError>,
}

/// Receivers for the three async boot tasks. Held on [`App`] so the
/// pre-render hook can drain them every loop iteration without
/// awaiting anything that isn't already ready.
#[derive(Default)]
pub struct BootChannels {
    pub discover_rx: Option<oneshot::Receiver<Vec<String>>>,
    pub diff_rx: Option<oneshot::Receiver<DiffPreload>>,
    pub watcher_rx: Option<mpsc::UnboundedReceiver<WatcherSpawnResult>>,
}

/// TUI application state.
pub struct App {
    config: Arc<Config>,
    items: Vec<Item>,
    selected: usize,
    quit: bool,
    executor: Option<Executor>,
    backend: Option<Arc<dyn Backend>>,
    /// Workspace devcontainer when opted-in (config `[devcontainer]
    /// enabled = true`). `None` keeps host-shell semantics for the
    /// terminals view; `Some` rewires `queue_new_shell` to drop into
    /// the devcontainer, and the event loop ensures it's up before
    /// yielding to tmux.
    devcontainer: Option<Arc<DevcontainerBackend>>,
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
    /// simple by just clearing on the next successful action). Mutate
    /// through [`App::flash`] / [`App::clear_flash`]; read through
    /// [`App::flash_message`]. The direct field is `pub(crate)` so
    /// callers in the TUI crate can still pattern-match in tests but
    /// the keel library doesn't expose it externally.
    pub(crate) flash_message: Option<String>,
    /// Active modal, if any. Replaces the four parallel
    /// `Option<...>` fields (palette / confirm / args_prompt /
    /// switcher) plus the redundant `Mode` enum — `None` ↔ normal
    /// mode, `Some(Modal::X)` ↔ X mode. Eliminates a class of
    /// mode/state desync bugs.
    modal: Option<Modal>,
    /// Queued one-shot side effects (worktree switch, tmux attach,
    /// lazygit handoff, kill window, BEL emit). Drained by the
    /// event loop in `drive` via [`Self::drain_commands`].
    commands: Vec<Command>,
    /// Inbox for background-task results. The sender half clones
    /// freely (every spawned task gets one); the App owns the
    /// receiver and drains it via [`Self::drain_messages`] once per
    /// render-loop tick. Lets handlers stay synchronous —
    /// "I/O happens elsewhere, the result lands here later."
    message_tx: mpsc::UnboundedSender<Message>,
    message_rx: mpsc::UnboundedReceiver<Message>,
    /// Cached project root so the switcher can prefill its path
    /// input with the current parent dir.
    project_root: PathBuf,
    /// Branch (or detached SHA / worktree dir basename) of the
    /// current worktree. Surfaced in the top bar so users always
    /// know which checkout they're acting on, regardless of view.
    branch: Option<String>,
    /// Active top-level view. Switched via the `T` / `g` / `c`
    /// global keybinds.
    view: View,
    /// Terminals view state (lazily initialised — empty Vec until
    /// the user first opens the view; the session name + tmux probe
    /// happen on first switch).
    terminals: TerminalsView,
    /// Diff view state. Lazily populated on first switch / refresh.
    diff: DiffView,
    /// Diagnostic messages flushed to stderr after the TUI exits.
    /// Used for issues that disappear from the flash slot before the
    /// user can read them (e.g. tmux session vanishing on detach
    /// gets clobbered by the very keypress that produced it).
    diagnostics: Vec<String>,
    /// Boot-task result channels. Populated by
    /// [`App::spawn_boot_tasks`] right after `App::new`; drained by
    /// the event loop's pre-render hook so the first frame can paint
    /// before service discovery / diff preload / watcher init finish.
    boot: BootChannels,
    /// Background state worker. Owns the periodic service status
    /// poll that used to block the render loop ~200 ms per tick on a
    /// busy compose daemon. Set when a non-None backend is wired in.
    worker: Option<crate::tui::worker::WorkerHandle>,
    /// Per-row rects for the control-center sidebar, in the same
    /// flat order as `self.items`. Populated by the renderer each
    /// frame; hit-tested by the mouse handler to map a click back to
    /// an item index.
    pub sidebar_item_rects: std::cell::RefCell<Vec<Rect>>,
    /// Rects for the Yes/No buttons of the active confirm dialog.
    /// Cleared (`None`) when no dialog is open or when the dialog
    /// just opened and hasn't been rendered yet.
    pub confirm_yes_rect: std::cell::Cell<Option<Rect>>,
    pub confirm_no_rect: std::cell::Cell<Option<Rect>>,
    /// Last click target + timestamp. A second click on the same
    /// target within [`DOUBLE_CLICK_WINDOW`] is treated as a
    /// double-click and activates the row. Reset on activation so
    /// triple-click can't re-fire.
    pub last_click: Option<(std::time::Instant, ClickTarget)>,
}

/// How long after a click we still treat a same-target click as a
/// double-click. 400 ms is the common desktop default and matches
/// what feels right when the user is intentionally double-tapping.
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

impl App {
    pub fn new(config: Arc<Config>) -> Self {
        let items = build_items(&config);
        let services = collect_service_panes(&config);
        let terminals = TerminalsView::default_for(&config);
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            config,
            items,
            selected: 0,
            quit: false,
            executor: None,
            backend: None,
            devcontainer: None,
            runs: BTreeMap::new(),
            lifecycle_run: None,
            services,
            watchers: BTreeMap::new(),
            flash_message: None,
            modal: None,
            commands: Vec::new(),
            message_tx,
            message_rx,
            project_root: PathBuf::from("."),
            branch: None,
            view: View::ControlCenter,
            terminals,
            diff: DiffView::default(),
            diagnostics: Vec::new(),
            boot: BootChannels::default(),
            worker: None,
            sidebar_item_rects: std::cell::RefCell::new(Vec::new()),
            confirm_yes_rect: std::cell::Cell::new(None),
            confirm_no_rect: std::cell::Cell::new(None),
            last_click: None,
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

    /// Tag the App with the current worktree's branch (or detached
    /// SHA / dir basename) so the top bar can surface it. Optional —
    /// the App still functions without it; the top bar just won't
    /// show a branch slot. Reset on every worktree hot-reload by the
    /// CLI's outer loop.
    pub fn with_branch(mut self, branch: Option<String>) -> Self {
        self.branch = branch;
        self
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
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

    /// Plumb in the opt-in devcontainer backend. When set, the
    /// Terminals view's new-shell sentinel drops into the devcontainer
    /// instead of running a host shell, and the event loop awaits
    /// `ensure_up` before yielding to tmux.
    pub fn with_devcontainer(mut self, devcontainer: Arc<DevcontainerBackend>) -> Self {
        self.devcontainer = Some(devcontainer);
        self
    }

    pub fn devcontainer(&self) -> Option<&Arc<DevcontainerBackend>> {
        self.devcontainer.as_ref()
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

    /// Set the sidebar selection to `idx`, clamped to the last valid
    /// position. No-op when the list is empty (clicks that land on a
    /// stale rect would otherwise out-of-bounds the selection).
    pub fn select_at(&mut self, idx: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = idx.min(self.items.len() - 1);
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
            ItemKind::Runtime => {
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
        self.clear_flash();
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
        match &self.modal {
            None => Mode::Normal,
            Some(Modal::Palette(_)) => Mode::Palette,
            Some(Modal::Confirm(_)) => Mode::Confirm,
            Some(Modal::ArgsPrompt(_)) => Mode::ArgsPrompt,
            Some(Modal::Switcher(_)) => Mode::WorktreeSwitcher,
        }
    }

    pub fn palette(&self) -> Option<&Palette> {
        match &self.modal {
            Some(Modal::Palette(p)) => Some(p),
            _ => None,
        }
    }

    pub fn open_palette(&mut self) {
        self.modal = Some(Modal::Palette(Palette::new(&self.items)));
    }

    pub fn close_palette(&mut self) {
        self.modal = None;
    }

    pub fn palette_mut(&mut self) -> Option<&mut Palette> {
        match &mut self.modal {
            Some(Modal::Palette(p)) => Some(p),
            _ => None,
        }
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
        let palette = self.palette()?;
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
        self.modal = Some(Modal::Confirm(ConfirmDialog {
            title: format!("`{}` is running", item.name),
            body: "Kill and restart?".into(),
            yes_focused: true,
            action: ConfirmAction::KillAndRestart { key },
        }));
    }

    pub fn confirm_dialog(&self) -> Option<&ConfirmDialog> {
        match &self.modal {
            Some(Modal::Confirm(d)) => Some(d),
            _ => None,
        }
    }

    pub fn confirm_dialog_mut(&mut self) -> Option<&mut ConfirmDialog> {
        match &mut self.modal {
            Some(Modal::Confirm(d)) => Some(d),
            _ => None,
        }
    }

    pub fn confirm_toggle_focus(&mut self) {
        if let Some(c) = self.confirm_dialog_mut() {
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
        self.modal = Some(Modal::ArgsPrompt(ArgsPrompt {
            item_name: item.name,
            kind: item.kind,
            input: String::new(),
        }));
    }

    pub fn args_prompt(&self) -> Option<&ArgsPrompt> {
        match &self.modal {
            Some(Modal::ArgsPrompt(p)) => Some(p),
            _ => None,
        }
    }

    fn args_prompt_mut(&mut self) -> Option<&mut ArgsPrompt> {
        match &mut self.modal {
            Some(Modal::ArgsPrompt(p)) => Some(p),
            _ => None,
        }
    }

    pub fn args_prompt_push_char(&mut self, c: char) {
        if let Some(p) = self.args_prompt_mut() {
            p.input.push(c);
        }
    }

    pub fn args_prompt_pop_char(&mut self) {
        if let Some(p) = self.args_prompt_mut() {
            p.input.pop();
        }
    }

    /// Resolve the args prompt: launch the row when `accept = true`,
    /// dismiss otherwise. Tokenises the input shell-style; returns
    /// the launch outcome (mirroring [`Self::confirm_palette`]).
    pub fn args_prompt_resolve(&mut self, accept: bool) -> Option<Result<(), LaunchRejection>> {
        let prompt = match self.modal.take() {
            Some(Modal::ArgsPrompt(p)) => p,
            other => {
                self.modal = other;
                return None;
            }
        };
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
        self.modal = Some(Modal::Switcher(WorktreeSwitcher {
            entries,
            selected,
            creating: None,
            row_rects: std::cell::RefCell::new(Vec::new()),
        }));
    }

    pub fn switcher(&self) -> Option<&WorktreeSwitcher> {
        match &self.modal {
            Some(Modal::Switcher(s)) => Some(s),
            _ => None,
        }
    }

    fn switcher_mut(&mut self) -> Option<&mut WorktreeSwitcher> {
        match &mut self.modal {
            Some(Modal::Switcher(s)) => Some(s),
            _ => None,
        }
    }

    /// Close the switcher without acting.
    pub fn close_switcher(&mut self) {
        self.modal = None;
    }

    pub fn switcher_select_next(&mut self) {
        if let Some(s) = self.switcher_mut() {
            let total = s.total_rows();
            if total > 0 {
                s.selected = (s.selected + 1).min(total - 1);
            }
        }
    }

    pub fn switcher_select_prev(&mut self) {
        if let Some(s) = self.switcher_mut() {
            s.selected = s.selected.saturating_sub(1);
        }
    }

    /// Set the switcher selection to `idx`, clamped to the last row
    /// (which is the synthetic "+ new worktree" sentinel). No-op when
    /// the switcher isn't open.
    pub fn switcher_select_at(&mut self, idx: usize) {
        if let Some(s) = self.switcher_mut() {
            let total = s.total_rows();
            if total > 0 {
                s.selected = idx.min(total - 1);
            }
        }
    }

    /// Resolve the switcher: if the selected row is an existing
    /// worktree, queue a hot-reload to its path and close the modal.
    /// If it's the synthetic "+ new worktree" row, signal the
    /// caller to fetch the branch list and open the create form
    /// (the branch fetch is async; we don't block the App in here).
    pub fn switcher_confirm(&mut self) -> SwitcherConfirm {
        let Some(s) = self.switcher() else {
            return SwitcherConfirm::NoOp;
        };
        if s.selected == s.new_row_index() {
            return SwitcherConfirm::OpenCreateForm;
        }
        let row = s.entries[s.selected].clone();
        if !row.is_current {
            self.queue(Command::SwitchWorktree(row.path));
        }
        self.close_switcher();
        SwitcherConfirm::Switched
    }

    /// Open the create form with a pre-fetched branch list. Caller
    /// runs `crate::runtime::list_branches` first (async) and hands
    /// the result in here (sync).
    ///
    /// `parent_override` is where new worktrees should be placed by
    /// default (Some = use this, None = fall back to the parent of
    /// the App's project root). The terminal layer typically passes
    /// `Some(git_toplevel.parent())` so a new worktree lands next
    /// to the repo regardless of where keel was invoked from —
    /// running keel in `<repo>/tmp/test` shouldn't make new
    /// worktrees land under `tmp/`.
    pub fn open_create_form(
        &mut self,
        branches: Vec<crate::runtime::BranchEntry>,
        parent_override: Option<std::path::PathBuf>,
    ) {
        // Compute parent before borrowing the switcher mutably so the
        // project-root immutable borrow doesn't overlap.
        //
        // `Path::new(".").parent()` returns `Some("")` (the empty
        // path), which makes `Path::join` drop the parent entirely.
        // Fall back to the project root when parent is empty too.
        let parent = parent_override.unwrap_or_else(|| match self.project_root.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => self.project_root.clone(),
        });
        let Some(s) = self.switcher_mut() else {
            return;
        };
        let filtered: Vec<usize> = (0..branches.len()).collect();
        let form = NewWorktreeForm {
            branch_input: String::new(),
            // Empty until the user types — auto-sync on first
            // keystroke. Showing `<parent>/` as a teaser would be
            // nice but ratatui can't render placeholder text.
            path_input: String::new(),
            path_dirty: false,
            parent,
            branches,
            filtered,
            selected: 0,
            focus: NewFormField::Branch,
            error: None,
        };
        s.creating = Some(form);
    }

    /// Mutate the open new-worktree form. Caller dispatches keys to
    /// these helpers from the switcher key handler.
    pub fn switcher_form_push_char(&mut self, c: char) {
        if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut()) {
            match form.focus {
                NewFormField::Path => {
                    form.path_input.push(c);
                    form.path_dirty = true;
                }
                NewFormField::Branch => {
                    form.branch_input.push(c);
                    refilter_branches(form);
                    sync_path_from_branch(form);
                }
            }
            form.error = None;
        }
    }

    pub fn switcher_form_pop_char(&mut self) {
        if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut()) {
            match form.focus {
                NewFormField::Path => {
                    form.path_input.pop();
                    form.path_dirty = true;
                }
                NewFormField::Branch => {
                    form.branch_input.pop();
                    refilter_branches(form);
                    sync_path_from_branch(form);
                }
            }
            form.error = None;
        }
    }

    pub fn switcher_form_toggle_focus(&mut self) {
        if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut()) {
            form.focus = match form.focus {
                NewFormField::Path => NewFormField::Branch,
                NewFormField::Branch => NewFormField::Path,
            };
        }
    }

    /// Move the highlighted branch / sentinel down. Only meaningful
    /// when focus is on the branch field (the path field has no
    /// list to navigate).
    pub fn switcher_form_select_next(&mut self) {
        if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut())
            && form.focus == NewFormField::Branch
        {
            let total = form.total_options();
            if total > 0 {
                form.selected = (form.selected + 1).min(total - 1);
            }
        }
    }

    pub fn switcher_form_select_prev(&mut self) {
        if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut())
            && form.focus == NewFormField::Branch
        {
            form.selected = form.selected.saturating_sub(1);
        }
    }

    pub fn switcher_form_cancel(&mut self) {
        if let Some(s) = self.switcher_mut() {
            s.creating = None;
        }
    }

    /// Snapshot the current form state without taking ownership;
    /// the caller invokes git from this data and reports back via
    /// [`Self::switcher_form_finish`].
    pub fn switcher_form_snapshot(&self) -> Option<NewWorktreeForm> {
        self.switcher().and_then(|s| s.creating.clone())
    }

    /// What the caller should ask `git worktree add` to do. Folds
    /// the focus + selection + sentinel logic into a single tuple
    /// the terminal layer can shell out from.
    pub fn switcher_form_resolve(&self) -> Option<NewWorktreeAction> {
        let form = self.switcher().and_then(|s| s.creating.as_ref())?;
        if form.path_input.trim().is_empty() {
            return None;
        }
        let branch = match form.focus {
            // In path-focus mode the branch list isn't relevant —
            // submit with whatever was last typed in the branch field.
            NewFormField::Path => {
                if form.branch_input.trim().is_empty() {
                    return None;
                }
                if form.branches.iter().any(|b| b.name == form.branch_input) {
                    BranchSpec::Existing(form.branch_input.clone())
                } else {
                    BranchSpec::CreateOff(form.branch_input.clone())
                }
            }
            NewFormField::Branch => {
                if form.selected < form.filtered.len() {
                    let idx = form.filtered[form.selected];
                    BranchSpec::Existing(form.branches[idx].name.clone())
                } else if form.show_create_sentinel() {
                    BranchSpec::CreateOff(form.branch_input.clone())
                } else {
                    return None;
                }
            }
        };
        Some(NewWorktreeAction {
            path: form.path_input.clone(),
            branch,
        })
    }

    /// Resolve the form after a `git worktree add` attempt. On Ok,
    /// queues a switch to the freshly-created path and closes the
    /// modal. On Err, surfaces the message inside the form so the
    /// user can fix and retry.
    pub fn switcher_form_finish(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                self.queue(Command::SwitchWorktree(path));
                self.close_switcher();
            }
            Err(msg) => {
                if let Some(form) = self.switcher_mut().and_then(|s| s.creating.as_mut()) {
                    form.error = Some(msg);
                }
            }
        }
    }

    pub fn terminals(&self) -> &TerminalsView {
        &self.terminals
    }

    pub fn terminals_mut(&mut self) -> &mut TerminalsView {
        &mut self.terminals
    }

    /// Stable list of rows the Terminals view shows: every service
    /// first, then every user shell window, then a sentinel "+ new
    /// shell" row. Service-attached windows (`svc:*`) are filtered
    /// out — they show up in the services group instead, never
    /// twice. Indexes returned here align with `selected`.
    pub fn terminals_rows(&self) -> Vec<TerminalsRow> {
        let mut rows = Vec::new();
        for name in self.services.keys() {
            rows.push(TerminalsRow::Service(name.clone()));
        }
        for w in &self.terminals.windows {
            if w.name.starts_with("svc:") {
                continue;
            }
            rows.push(TerminalsRow::Window(w.clone()));
        }
        rows.push(TerminalsRow::NewSentinel);
        rows
    }

    /// Replace the cached tmux window list and clamp selection to
    /// the new row count. Wraps [`TerminalsView::set_windows`] with
    /// the row total (services + windows + sentinel) the view itself
    /// can't know, and translates the view's bell-edge flag into a
    /// queued [`Command::EmitBell`] so the event loop can write the
    /// BEL after the next render.
    pub fn terminals_set_windows(&mut self, windows: Vec<TmuxWindow>) {
        let non_service_windows = windows.iter().filter(|w| !w.name.starts_with("svc:")).count();
        // services count + window rows + 1 sentinel
        let total_after = self.services.len() + non_service_windows + 1;
        self.terminals.set_windows(windows, total_after);
        if self.terminals.take_pending_bell() {
            self.queue(Command::EmitBell);
        }
    }

    /// Drop any tmux snapshots the worker queued while the user was
    /// detached, without applying them. Pairs with
    /// [`TerminalsView::silence_next_bell`] to keep stale "had-bell
    /// mid-attach" snapshots from re-firing the BEL on return.
    pub fn discard_pending_tmux_snapshots(&mut self) {
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        loop {
            match w.snap_rx.try_recv() {
                Ok(crate::tui::worker::WorkerSnapshot::TmuxWindows(_)) => continue,
                Ok(crate::tui::worker::WorkerSnapshot::ServiceStatus { name, status }) => {
                    if let Some(pane) = self.services.get_mut(&name) {
                        pane.status = Some(status);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.worker = None;
                    break;
                }
            }
        }
    }

    pub fn terminals_select_next(&mut self) {
        let total = self.terminals_rows().len();
        self.terminals.select_next(total);
    }

    /// Set the Terminals-view selection to `idx`, clamped to the last
    /// row. Wrapper around [`TerminalsView::select_at`] that supplies
    /// the row total.
    pub fn terminals_select_at(&mut self, idx: usize) {
        let total = self.terminals_rows().len();
        self.terminals.select_at(idx, total);
    }

    /// Resolve the selected row in the Terminals view.
    ///
    ///   service row   → queue an attach into the compose service
    ///   window row    → queue an attach to that tmux window
    ///   `+ new shell` → queue an attach for a brand-new shell;
    ///                   the attach handler creates the window
    ///                   when the user lands in it. No name, no
    ///                   command — tmux's automatic-rename takes
    ///                   over once a program runs in the pane.
    pub fn terminals_confirm(&mut self) {
        let rows = self.terminals_rows();
        let Some(row) = rows.get(self.terminals.selected).cloned() else {
            return;
        };
        match row {
            TerminalsRow::Service(name) => {
                if let Err(msg) = self.queue_service_attach(&name) {
                    self.flash(msg);
                }
            }
            TerminalsRow::Window(window) => {
                let session = self.terminals.session_name.clone();
                self.queue(Command::AttachTmux(AttachRequest {
                    session,
                    window: window.index.to_string(),
                    create_with: None,
                    ensure: None,
                    window_name: None,
                    window_options: Vec::new(),
                }));
            }
            TerminalsRow::NewSentinel => {
                self.queue_new_shell();
            }
        }
    }

    /// Spawn a fresh shell window in the worktree's tmux session
    /// and queue the attach. Used by both the `+ new shell`
    /// sentinel row and the `n` shortcut so the keybind and the
    /// list entry produce identical behaviour.
    ///
    /// When a devcontainer is configured, the shell lands inside the
    /// devcontainer (via `docker exec`) at the workspaceFolder. The
    /// event loop ensures the container is up before yielding to
    /// tmux — this fn stays synchronous to keep the App's event-loop
    /// borrowing simple.
    pub fn queue_new_shell(&mut self) {
        let session = self.terminals.session_name.clone();
        let (create_with, ensure, window_name, window_options) =
            if let Some(dc) = &self.devcontainer {
                let workspace = dc.workspace_folder().to_string();
                let cmd = format!(
                    "docker exec -it {name} sh -c 'cd {cwd} && exec ${{SHELL:-/bin/sh}}'",
                    name = dc.container_name(),
                    cwd = shell_escape(workspace.clone()),
                );
                // Explicit window name (`dc`) keeps tmux's
                // automatic-rename from rewriting to "docker" the
                // moment the foreground process starts. The user
                // option carries the in-container workspace path so
                // the sidebar can show it instead of the docker
                // client's host-side pwd.
                (
                    Some(cmd),
                    Some(EnsureDevcontainer {
                        backend: Arc::clone(dc),
                    }),
                    Some("dc".to_string()),
                    vec![("@keel_workspace".to_string(), workspace)],
                )
            } else {
                let cmd = format!(
                    "cd {} && exec ${{SHELL:-/bin/sh}}",
                    shell_escape(self.project_root.display().to_string())
                );
                (Some(cmd), None, None, Vec::new())
            };
        // `keel-new` is the sentinel marker — the attach handler
        // unconditionally calls `tmux new-window` for it, so each
        // press creates a distinct window. tmux's automatic-rename
        // overwrites the placeholder name once `$SHELL` starts —
        // unless `window_name` is set, which is the devcontainer
        // path's defence against everything becoming "docker".
        self.queue(Command::AttachTmux(AttachRequest {
            session,
            window: "keel-new".to_string(),
            create_with,
            ensure,
            window_name,
            window_options,
        }));
    }

    /// Open a confirmation modal for killing the selected window.
    /// Services and the sentinel aren't killable; this no-ops on
    /// those. The actual `tmux kill-window` shell-out is queued
    /// (via `pending_kill_window`) only after the user accepts.
    pub fn terminals_kill_selected(&mut self) {
        let rows = self.terminals_rows();
        let Some(row) = rows.get(self.terminals.selected) else {
            return;
        };
        let TerminalsRow::Window(window) = row else {
            return;
        };
        let session = self.terminals.session_name.clone();
        let name = window.name.clone();
        let index = window.index;
        self.modal = Some(Modal::Confirm(ConfirmDialog {
            title: format!("close `{name}`?"),
            body: "the tmux window and any running processes will end.".into(),
            yes_focused: true,
            action: ConfirmAction::KillTmuxWindow {
                session,
                index,
                name,
            },
        }));
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
        self.queue(Command::AttachTmux(AttachRequest {
            session,
            window,
            create_with,
            ensure: None,
            window_name: None,
            window_options: Vec::new(),
        }));
        self.view = View::Terminals;
        Ok(())
    }

    /// Buffer a diagnostic message to print after the TUI exits.
    /// Use for issues whose flash would be clobbered immediately
    /// (e.g. tmux post-detach failures: the very keypress that
    /// caused the detach also clears `flash`).
    pub fn diagnostic(&mut self, msg: impl Into<String>) {
        self.diagnostics.push(msg.into());
    }

    pub fn drain_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Queue a one-shot side effect for the event loop to drain
    /// between ticks. Handlers call this from synchronous code paths
    /// that can't await — the loop owns the async work.
    pub fn queue(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Hand the queued commands to the event loop. Called once per
    /// tick from [`crate::tui::terminal::drive`]; the loop matches
    /// each variant and runs the corresponding side effect.
    pub fn drain_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    /// Clone the message sender. Handed to spawned tokio tasks so
    /// their results can be posted back without holding a borrow on
    /// App across the await.
    pub fn message_tx(&self) -> mpsc::UnboundedSender<Message> {
        self.message_tx.clone()
    }

    /// Drain pending inbox messages into App state. Non-blocking;
    /// called once per render-loop tick from the pre-render drain.
    /// Each variant has a sync handler so the loop never awaits
    /// here.
    pub fn drain_messages(&mut self) {
        while let Ok(msg) = self.message_rx.try_recv() {
            self.apply_message(msg);
        }
    }

    fn apply_message(&mut self, msg: Message) {
        match msg {
            Message::TmuxProbeResult(ok) => {
                self.terminals.set_tmux_available(ok);
            }
            Message::DiffReloaded(preload) => {
                self.apply_diff_preload(preload);
                // Cascade: now that the file list is fresh, kick off a
                // per-file body load for the selected file (unless its
                // cache already has it from a prior session). Same
                // shape as the old `ensure_diff_for_selected` tail of
                // `ensure_diff_loaded`.
                self.request_diff_for_selected();
            }
            Message::DiffFileLoaded { path, lines } => {
                self.diff.set_cache(path, lines);
                // If the user has flipped to read mode in the
                // meantime, kick off the read load too. Annotation
                // needs the diff cache to be populated first; both
                // race in parallel without sequencing.
                if self.diff.body_mode == BodyMode::Read {
                    self.request_read_for_selected();
                }
            }
            Message::ReadFileLoaded { path, lines } => {
                // Annotate against the diff cache before storing —
                // matches the pre-actor flow where
                // `ensure_read_for_selected` ran annotate inline.
                let annotated = match self.diff.cache_for(&path) {
                    Some(diff_lines) => {
                        crate::tui::views::diff::git::annotate_read_with_diff(lines, diff_lines)
                    }
                    None => lines,
                };
                self.diff.set_read_cache(path, annotated);
            }
        }
    }

    /// Spawn a background tmux availability probe. The result lands
    /// on the message inbox; [`Self::drain_messages`] applies it via
    /// [`Message::TmuxProbeResult`]. No-op when the probe has
    /// already produced a result (the view's `tmux_available` is
    /// `Some(_)`).
    pub fn request_tmux_probe(&self) {
        if self.terminals.tmux_available.is_some() {
            return;
        }
        let tx = self.message_tx();
        tokio::spawn(async move {
            let ok = tokio::process::Command::new("tmux")
                .arg("-V")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            let _ = tx.send(Message::TmuxProbeResult(ok));
        });
    }

    /// Spawn a background top-level diff reload (anchor refresh +
    /// `git diff --name-status` + numstat + ls-files-others).
    /// Result lands on the inbox as [`Message::DiffReloaded`];
    /// `apply_message` folds it onto `DiffView` and cascades a
    /// per-file body load for the selected file.
    ///
    /// Spawns even when a boot preload is still in flight — the
    /// later result wins (insertion order into the channel). For
    /// user-pressed `r` that's the desired behavior; for first-entry
    /// the boot preload usually lands first anyway.
    pub fn request_diff_reload(&self) {
        let tx = self.message_tx();
        let project_root = self.project_root.clone();
        let configured_base = self.config.diff.base.clone();
        let lazygit_available = self.diff.lazygit_available;
        tokio::spawn(async move {
            let trunk =
                crate::runtime::detect_trunk(&project_root, configured_base.as_deref()).await;
            let anchor = match trunk.as_deref() {
                Some(t) => crate::runtime::merge_base(&project_root, t).await,
                None => None,
            };
            let branch = crate::tui::views::diff::git::current_branch(&project_root).await;
            let anchor_short = anchor
                .as_deref()
                .map(|sha| sha.chars().take(7).collect::<String>());
            let files = crate::tui::views::diff::git::load_diff_files(
                &project_root,
                anchor.as_deref(),
            )
            .await;
            let _ = tx.send(Message::DiffReloaded(DiffPreload {
                trunk,
                anchor,
                anchor_short,
                branch,
                lazygit_available,
                files,
            }));
        });
    }

    /// Spawn a background per-file diff body load for the currently-
    /// selected file. No-op when the cache already has the file's
    /// diff body, or when no file is selected. The result lands as
    /// [`Message::DiffFileLoaded`].
    pub fn request_diff_for_selected(&self) {
        let Some(file) = self.diff.selected_file().cloned() else {
            return;
        };
        if self.diff.cache_for(&file.path).is_some() {
            return;
        }
        let tx = self.message_tx();
        let project_root = self.project_root.clone();
        let anchor = self.diff.anchor.clone();
        tokio::spawn(async move {
            let lines = crate::tui::views::diff::git::load_diff_for_file(
                &project_root,
                &file,
                anchor.as_deref(),
            )
            .await;
            let _ = tx.send(Message::DiffFileLoaded {
                path: file.path,
                lines,
            });
        });
    }

    /// Spawn a background per-file read-mode body load for the
    /// currently-selected file. No-op when the read cache already
    /// has the file. The result lands as [`Message::ReadFileLoaded`]
    /// and is annotated against the diff cache before storing.
    pub fn request_read_for_selected(&self) {
        let Some(file) = self.diff.selected_file().cloned() else {
            return;
        };
        if self.diff.read_cache_for(&file.path).is_some() {
            return;
        }
        // If the diff cache for this file isn't populated yet, kick
        // that off too — the read annotator needs it. The two loads
        // race in parallel; whichever finishes second triggers the
        // annotation (`apply_message` for ReadFileLoaded checks
        // `diff.cache_for(...)` at that moment).
        if self.diff.cache_for(&file.path).is_none() {
            self.request_diff_for_selected();
        }
        let tx = self.message_tx();
        let project_root = self.project_root.clone();
        let anchor = self.diff.anchor.clone();
        tokio::spawn(async move {
            let lines = crate::tui::views::diff::git::load_read_for_file(
                &project_root,
                &file,
                anchor.as_deref(),
            )
            .await;
            let _ = tx.send(Message::ReadFileLoaded {
                path: file.path,
                lines,
            });
        });
    }

    /// Show a transient status banner. Most action paths flash on
    /// rejection; the slot clears on the next render-loop tick or
    /// when [`Self::clear_flash`] runs.
    pub fn flash(&mut self, msg: impl Into<String>) {
        self.flash_message = Some(msg.into());
    }

    /// Clear the flash slot. Called by the event loop on each tick
    /// so banners decay between keystrokes.
    pub fn clear_flash(&mut self) {
        self.flash_message = None;
    }

    /// Current flash banner text, or `None` when no banner is active.
    pub fn flash_message(&self) -> Option<&str> {
        self.flash_message.as_deref()
    }

    pub fn diff(&self) -> &DiffView {
        &self.diff
    }

    pub fn diff_mut(&mut self) -> &mut DiffView {
        &mut self.diff
    }

    pub fn request_lazygit(&mut self) {
        self.queue(Command::OpenLazygit);
    }

    pub fn confirm_resolve(&mut self, accept: bool) -> Option<LaunchRejection> {
        let d = match self.modal.take() {
            Some(Modal::Confirm(d)) => d,
            other => {
                self.modal = other;
                return None;
            }
        };
        // Clear the dialog's button rects so a click that lands after
        // the dialog closed can't match against a stale layout.
        self.confirm_yes_rect.set(None);
        self.confirm_no_rect.set(None);
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
            ConfirmAction::KillTmuxWindow { session, index, .. } => {
                self.queue(Command::KillTmuxWindow(KillWindow { session, index }));
                None
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
                if let Some(idx) = self.items.iter().position(|i| i.kind == ItemKind::Runtime) {
                    self.selected = idx;
                }
                Ok(())
            }
            Err(e) => Err(LaunchRejection::NotRunnable(format!("{e}"))),
        }
    }

    /// Poll completion for every active run plus the lifecycle slot.
    /// Each `RunState` short-circuits on already-done. Pokes the
    /// worker when the lifecycle slot transitions to done so service
    /// indicators flip without waiting for the next 2-second tick.
    pub fn poll_runs(&mut self) {
        for run in self.runs.values_mut() {
            run.poll_completion();
        }
        if let Some(run) = self.lifecycle_run.as_mut() {
            let was_done = run.is_done();
            run.poll_completion();
            if !was_done && run.is_done() {
                self.poke_worker_status();
            }
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

    /// Kick off the three boot operations on background tasks: service
    /// discovery (compose `list_services`), diff preload (trunk +
    /// merge-base + name-status + numstat + ls-files), and watcher
    /// pane spawning (notify + globset compile per pane). Returns
    /// immediately; results land via [`Self::drain_boot_results`].
    ///
    /// Designed so the first frame can paint before any of these
    /// finish — boot time is no longer gated on a slow Docker daemon
    /// or a cold git cache.
    pub fn spawn_boot_tasks(&mut self, project_root: &Path) {
        // 0. Spawn the background state worker if we have a backend.
        // Seeded with whatever services the config already declared
        // so the first poll tick has work to do without waiting for
        // discovery to land. Also seeds the tmux session name so the
        // worker can poll #{window_bell_flag} for the Terminals view.
        if let Some(backend) = self.backend.as_ref().map(Arc::clone) {
            let initial = self.services.keys().cloned().collect();
            let handle = crate::tui::worker::spawn(backend, initial);
            let _ = handle
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::SetTmuxSession(Some(
                    self.terminals.session_name.clone(),
                )));
            self.worker = Some(handle);
        }

        // 1. Service discovery — one-shot.
        if let Some(backend) = self.backend.as_ref().map(Arc::clone) {
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                // Empty Vec on failure: caller treats discovery as
                // best-effort (the auto-discovered group just stays
                // empty). Matches the pre-refactor `discover_services`
                // behaviour where a `list_services` Err was a no-op.
                let names = backend.list_services().await.unwrap_or_default();
                let _ = tx.send(names);
            });
            self.boot.discover_rx = Some(rx);
        }

        // 2. Diff preload — one-shot.
        let project_root_owned = project_root.to_path_buf();
        let configured_base = self.config.diff.base.clone();
        let (diff_tx, diff_rx) = oneshot::channel();
        tokio::spawn(async move {
            let trunk =
                crate::runtime::detect_trunk(&project_root_owned, configured_base.as_deref()).await;
            let anchor = match trunk.as_deref() {
                Some(t) => crate::runtime::merge_base(&project_root_owned, t).await,
                None => None,
            };
            let branch = crate::tui::views::diff::git::current_branch(&project_root_owned).await;
            let anchor_short = anchor
                .as_deref()
                .map(|sha| sha.chars().take(7).collect::<String>());
            let lazygit_available = crate::tui::lazygit::is_available();
            let files =
                crate::tui::views::diff::git::load_diff_files(&project_root_owned, anchor.as_deref()).await;
            let _ = diff_tx.send(DiffPreload {
                trunk,
                anchor,
                anchor_short,
                branch,
                lazygit_available,
                files,
            });
        });
        self.boot.diff_rx = Some(diff_rx);

        // 3. Watcher panes — mpsc, one item per pane. Spawning is sync
        // (globset compile + notify init) so we hop to `spawn_blocking`;
        // each pane streams back independently so a slow pane (notify
        // init on a deep tree) doesn't hold up the others.
        let watcher_specs: Vec<(usize, String, Vec<String>, u64)> = self
            .config
            .ui
            .panes
            .iter()
            .enumerate()
            .filter_map(|(idx, pane)| match pane {
                UiPane::Watcher {
                    glob,
                    on_change,
                    debounce_ms,
                    ..
                } => Some((idx, on_change.clone(), glob.clone(), *debounce_ms)),
                _ => None,
            })
            .collect();
        if !watcher_specs.is_empty() {
            let (w_tx, w_rx) = mpsc::unbounded_channel();
            let project_root_for_watchers = project_root.to_path_buf();
            tokio::task::spawn_blocking(move || {
                // Replicate `unique_watcher_name` locally — we don't
                // hold a reference to App from this thread, so we
                // dedupe against names we've already emitted.
                let mut emitted: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for (idx, on_change, glob, debounce_ms) in watcher_specs {
                    let primary = format!("watch:{on_change}");
                    let name = if emitted.insert(primary.clone()) {
                        primary
                    } else {
                        let fallback = format!("watch:{on_change}:{idx}");
                        emitted.insert(fallback.clone());
                        fallback
                    };
                    let pane = WatcherPane::spawn(
                        name.clone(),
                        on_change,
                        glob,
                        Duration::from_millis(debounce_ms),
                        &project_root_for_watchers,
                    );
                    if w_tx.send(WatcherSpawnResult { name, pane }).is_err() {
                        // Receiver dropped — TUI exited mid-spawn.
                        return;
                    }
                }
            });
            self.boot.watcher_rx = Some(w_rx);
        }
    }

    /// Non-blocking drain of any boot-task results that have landed
    /// since the last call. Called once per loop iteration in the
    /// pre-render hooks so the sidebar / header fill in as soon as
    /// each task completes — no extra wake needed.
    pub fn drain_boot_results(&mut self) {
        // Service discovery.
        if let Some(rx) = self.boot.discover_rx.as_mut()
            && let Ok(names) = rx.try_recv()
        {
            self.boot.discover_rx = None;
            let mut added = false;
            for name in names {
                if !self.services.contains_key(&name) {
                    self.services
                        .insert(name.clone(), ServicePane::new(name.clone()));
                    added = true;
                }
            }
            if added {
                self.items = build_items_from(&self.config, &self.services, &self.watchers);
                // Refresh the worker's service set so the auto-
                // discovered rows start receiving status updates.
                if let Some(w) = self.worker.as_ref() {
                    let _ = w
                        .cmd_tx
                        .send(crate::tui::worker::WorkerCommand::SetServices(
                            self.services.keys().cloned().collect(),
                        ));
                }
            }
        }

        // Diff preload.
        if let Some(rx) = self.boot.diff_rx.as_mut()
            && let Ok(preload) = rx.try_recv()
        {
            self.boot.diff_rx = None;
            self.apply_diff_preload(preload);
        }

        // Watcher panes — drain everything that arrived this tick.
        if let Some(rx) = self.boot.watcher_rx.as_mut() {
            let mut added = false;
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(result) => match result.pane {
                        Ok(pane) => {
                            self.watchers.insert(result.name, pane);
                            added = true;
                        }
                        Err(e) => {
                            tracing::warn!("watcher pane `{}` failed to start: {e}", result.name);
                        }
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected {
                self.boot.watcher_rx = None;
            }
            if added {
                self.items = build_items_from(&self.config, &self.services, &self.watchers);
            }
        }
    }

    fn apply_diff_preload(&mut self, preload: DiffPreload) {
        self.diff.set_anchor(
            preload.trunk,
            preload.anchor,
            preload.branch,
            preload.anchor_short,
        );
        self.diff.set_lazygit_available(preload.lazygit_available);
        match preload.files {
            Ok(files) => self.diff.set_files(files),
            Err(msg) => self.diff.set_error(msg),
        }
    }

    /// Apply any worker snapshots that have arrived since the last
    /// call. Non-blocking — replaces the inline
    /// `refresh_service_status` await that used to shell out to
    /// compose for each service on every pre-render tick.
    pub fn drain_worker_snapshots(&mut self) {
        // Coalesce tmux-window snapshots: only the most recent
        // reflects the current state, and applying older ones first
        // would briefly flicker stale data on the next render.
        let mut latest_tmux: Option<Vec<TmuxWindow>> = None;
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        loop {
            match w.snap_rx.try_recv() {
                Ok(crate::tui::worker::WorkerSnapshot::ServiceStatus { name, status }) => {
                    if let Some(pane) = self.services.get_mut(&name) {
                        pane.status = Some(status);
                    }
                }
                Ok(crate::tui::worker::WorkerSnapshot::TmuxWindows(windows)) => {
                    latest_tmux = Some(windows);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.worker = None;
                    break;
                }
            }
        }
        if let Some(windows) = latest_tmux {
            self.terminals_set_windows(windows);
        }
    }

    /// Signal the worker that service-affecting state just changed
    /// (e.g. compose `up`/`down`/`restart`) so it polls immediately
    /// rather than waiting for the next interval tick. Best-effort —
    /// the next tick would still pick the change up within ~2 s.
    pub fn poke_worker_status(&self) {
        if let Some(w) = self.worker.as_ref() {
            let _ = w
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::PokeServiceStatus);
        }
    }

    /// Signal the worker to refresh the tmux window list now —
    /// after an attach return, kill, or view entry, so the row list
    /// (and bell flags) feel reactive instead of waiting for the
    /// 1 s tick.
    pub fn poke_worker_tmux_windows(&self) {
        if let Some(w) = self.worker.as_ref() {
            let _ = w
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::PokeTmuxWindows);
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
    pub fn tick_watchers(&mut self) {
        let Some(executor) = self.executor.clone() else {
            return;
        };
        for pane in self.watchers.values_mut() {
            pane.tick(&executor);
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

/// Returns the row label for the runtime backend, or `None` when
/// no container backend is configured (`backend = "none"`). The row
/// label is the backend name the user typed in `[runtime]
/// backend = "..."` so the sidebar mirrors their config.
fn runtime_row_label(config: &Config) -> Option<&'static str> {
    use crate::config::model::Backend as B;
    match config.runtime.backend {
        B::None => None,
        B::Compose => Some("compose"),
        B::Docker => Some("docker"),
        B::Podman => Some("podman"),
    }
}

/// Reconstruct the sidebar item list from live state. The order is
/// stable: runtime (when configured), services (declared first in
/// keel.toml order, then any auto-discovered ones), watchers,
/// recipes, scripts.
fn build_items_from(
    config: &Config,
    services: &BTreeMap<String, ServicePane>,
    watchers: &BTreeMap<String, WatcherPane>,
) -> Vec<Item> {
    let mut items = Vec::new();

    // Runtime row first (when a backend is configured) — this is the
    // canonical home for backend lifecycle output (compose `U` / `D`
    // / `R` / `S`). One row, fixed name, top of the sidebar.
    if let Some(name) = runtime_row_label(config) {
        items.push(Item {
            name: name.to_string(),
            kind: ItemKind::Runtime,
        });
    }

    let mut emitted_services: std::collections::BTreeSet<&str> = Default::default();

    // Declared services first, in keel.toml [[ui.pane]] order.
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
    use crate::tui::views::diff::state::{
        BodyMode, DiffFocus, DiffLine, DiffLineKind, DiffStatus, ReadLine, ReadLineKind,
    };
    use pretty_assertions::assert_eq;

    /// Drain the command queue and return the first queued
    /// [`Command::AttachTmux`] payload (or `None`). Test-only
    /// convenience so callers don't have to write the match every
    /// time.
    fn drained_attach(app: &mut App) -> Option<AttachRequest> {
        app.drain_commands().into_iter().find_map(|c| match c {
            Command::AttachTmux(r) => Some(r),
            _ => None,
        })
    }

    fn drained_switch(app: &mut App) -> Option<PathBuf> {
        app.drain_commands().into_iter().find_map(|c| match c {
            Command::SwitchWorktree(p) => Some(p),
            _ => None,
        })
    }

    fn drained_kill_window(app: &mut App) -> Option<KillWindow> {
        app.drain_commands().into_iter().find_map(|c| match c {
            Command::KillTmuxWindow(k) => Some(k),
            _ => None,
        })
    }

    fn drained_emit_bell(app: &mut App) -> bool {
        app.drain_commands()
            .into_iter()
            .any(|c| matches!(c, Command::EmitBell))
    }

    /// Test cfg with no container backend, so the synthetic container
    /// row doesn't pollute item-count / index assertions for tests
    /// whose subject is unrelated.
    fn cfg() -> Arc<Config> {
        Arc::new(
            crate::config::parse_str(
                r#"
                [runtime]
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
            crate::config::parse_str(
                r#"
                [runtime]
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
            crate::config::parse_str(
                r#"
                [runtime]
                backend = "compose"

                [command.up]
                run = "true"
            "#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert_eq!(app.items()[0].kind, ItemKind::Runtime);
        assert_eq!(app.items()[0].name, "compose");
    }

    #[test]
    fn no_container_row_when_backend_none() {
        let cfg = Arc::new(
            crate::config::parse_str(
                r#"
                [runtime]
                backend = "none"
                [command.up]
                run = "true"
            "#,
            )
            .unwrap(),
        );
        let app = App::new(cfg);
        assert!(app.items().iter().all(|i| i.kind != ItemKind::Runtime));
    }

    #[test]
    fn collect_service_panes_picks_up_ui_services() {
        let cfg = crate::config::parse_str(
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
            crate::config::parse_str(
                r#"
                [runtime]
                backend = "none"

                [[ui.pane]]
                type = "service"
                service = "app"
            "#,
            )
            .unwrap(),
        );
        let backend: Arc<dyn crate::container::Backend> =
            Arc::new(crate::container::null::NullBackend);
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
            crate::config::parse_str(
                r#"[runtime]
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
            crate::config::parse_str(
                r#"
                [runtime]
                backend = "none"

                [command.shell]
                in = "app"
                run = "/bin/sh"
            "#,
            )
            .unwrap(),
        );
        let backend: Arc<dyn crate::container::Backend> =
            Arc::new(crate::container::null::NullBackend);
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
        let outcome = app.switcher_confirm();
        assert_eq!(outcome, SwitcherConfirm::Switched);
        assert!(drained_switch(&mut app).is_none());
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn switcher_confirm_on_other_row_queues_switch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        let outcome = app.switcher_confirm();
        assert_eq!(outcome, SwitcherConfirm::Switched);
        assert_eq!(
            drained_switch(&mut app),
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
    fn switcher_confirm_on_new_row_signals_open_form() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        let outcome = app.switcher_confirm();
        assert_eq!(outcome, SwitcherConfirm::OpenCreateForm);
        // The form isn't populated yet — terminal layer fetches
        // branches async, then calls open_create_form.
        assert!(app.switcher().unwrap().creating.is_none());
        app.open_create_form(Vec::new(), None);
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
        app.open_create_form(Vec::new(), None);
        app.switcher_form_finish(Ok(PathBuf::from("/repo-new")));
        assert_eq!(drained_switch(&mut app), Some(PathBuf::from("/repo-new")));
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn switcher_form_finish_err_keeps_form_open_with_error() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.open_create_form(Vec::new(), None);
        app.switcher_form_finish(Err("git: branch already exists".into()));
        assert_eq!(app.mode(), Mode::WorktreeSwitcher);
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.error.as_deref(), Some("git: branch already exists"));
    }

    #[test]
    fn terminals_default_has_no_windows() {
        let app = App::new(cfg());
        assert!(app.terminals().windows.is_empty());
        // Even with no windows, the sidebar still has the
        // sentinel row to launch a shell from.
        let rows = app.terminals_rows();
        assert!(matches!(
            rows.last().unwrap(),
            crate::tui::app::TerminalsRow::NewSentinel
        ));
    }

    #[test]
    fn terminals_set_windows_populates_rows() {
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![
            crate::tui::app::TmuxWindow {
                index: 0,
                name: "zsh".into(),
                cwd: None,
                has_bell: false,
                workspace: None,
            },
            crate::tui::app::TmuxWindow {
                index: 1,
                name: "vim".into(),
                cwd: None,
                has_bell: false,
                workspace: None,
            },
        ]);
        let rows = app.terminals_rows();
        // No services in this cfg + 2 windows + sentinel = 3 rows.
        assert_eq!(rows.len(), 3);
        assert!(matches!(
            rows[0],
            crate::tui::app::TerminalsRow::Window(ref w) if w.name == "zsh"
        ));
    }

    #[test]
    fn terminals_set_windows_filters_service_windows() {
        // svc:* windows are surfaced via the service row instead;
        // they should not appear in the terminals list.
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![
            crate::tui::app::TmuxWindow {
                index: 0,
                name: "zsh".into(),
                cwd: None,
                has_bell: false,
                workspace: None,
            },
            crate::tui::app::TmuxWindow {
                index: 1,
                name: "svc:app".into(),
                cwd: None,
                has_bell: false,
                workspace: None,
            },
        ]);
        let rows = app.terminals_rows();
        // Only the zsh window + sentinel — svc:app is excluded.
        assert_eq!(rows.len(), 2);
        assert!(matches!(
            rows[0],
            crate::tui::app::TerminalsRow::Window(ref w) if w.name == "zsh"
        ));
        assert!(matches!(
            rows[1],
            crate::tui::app::TerminalsRow::NewSentinel
        ));
    }

    /// First time we see a window's bell flipped on: arm
    /// `pending_bell_emit` so the render loop forwards BEL.
    #[test]
    fn terminals_set_windows_arms_bell_on_false_to_true_transition() {
        let mut app = App::new(cfg());
        // Baseline: no bell.
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        assert!(!drained_emit_bell(&mut app));
        // Bell flips on.
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        assert!(drained_emit_bell(&mut app), "fresh bell should arm emit");
        // take_pending_bell clears the flag.
        assert!(!drained_emit_bell(&mut app));
    }

    /// A window whose bell stays set across snapshots should NOT
    /// re-arm — otherwise we'd beep every worker tick (~1s) until
    /// the user attaches.
    #[test]
    fn terminals_set_windows_does_not_rearm_bell_while_set() {
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        assert!(drained_emit_bell(&mut app), "first observation arms");
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        assert!(!drained_emit_bell(&mut app), "still-set bell should not re-arm");
    }

    /// `silence_next_bell` resyncs the baseline without arming —
    /// used right after attach return so mid-attach bells (already
    /// played by tmux's `bell-action any`) don't double-fire.
    #[test]
    fn silence_next_bell_suppresses_one_emit_and_then_rearms() {
        let mut app = App::new(cfg());
        // Establish "no bell" baseline.
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals_mut().silence_next_bell();
        // This transition would normally arm — silence drops it.
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        assert!(
            !drained_emit_bell(&mut app),
            "silence flag should suppress this one emit"
        );
        // Subsequent transitions arm again. Clear, then re-fire.
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        assert!(
            drained_emit_bell(&mut app),
            "silence is single-shot — next transition should arm"
        );
    }

    #[test]
    fn terminals_confirm_on_sentinel_queues_new_shell() {
        let mut app = App::new(cfg());
        // Single row (sentinel). selected = 0.
        app.terminals_confirm();
        let req = drained_attach(&mut app).unwrap();
        assert_eq!(req.window, "keel-new");
        assert!(req.create_with.as_deref().unwrap().contains("SHELL"));
    }

    #[test]
    fn terminals_confirm_on_window_queues_attach_by_index() {
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 3,
            name: "vim".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals.selected = 0;
        app.terminals_confirm();
        let req = drained_attach(&mut app).unwrap();
        assert_eq!(req.window, "3");
        assert!(req.create_with.is_none());
    }

    #[test]
    fn queue_new_shell_matches_sentinel_confirm() {
        // The `n` shortcut should produce the same AttachRequest
        // as confirming the sentinel row — both go through
        // `queue_new_shell`. This asserts they match byte-for-byte
        // so regressing one would catch the other in tests.
        let mut from_shortcut = App::new(cfg());
        from_shortcut.queue_new_shell();
        let req_shortcut = drained_attach(&mut from_shortcut).unwrap();

        let mut from_sentinel = App::new(cfg());
        // Single sentinel row at index 0 in the empty cfg.
        from_sentinel.terminals_confirm();
        let req_sentinel = drained_attach(&mut from_sentinel).unwrap();

        assert_eq!(req_shortcut.session, req_sentinel.session);
        assert_eq!(req_shortcut.window, req_sentinel.window);
        assert_eq!(req_shortcut.create_with, req_sentinel.create_with);
        assert_eq!(req_shortcut.window, "keel-new");
    }

    #[test]
    fn terminals_kill_selected_opens_confirm_modal() {
        // Killing now goes through a confirm modal — no immediate
        // pending_kill_window. The shell-out fires only after the
        // user accepts via confirm_resolve(true).
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals.selected = 0;
        app.terminals_kill_selected();
        assert_eq!(app.mode(), Mode::Confirm);
        assert!(drained_kill_window(&mut app).is_none());
        let dialog = app.confirm_dialog().unwrap();
        assert!(dialog.title.contains("zsh"));
        assert!(dialog.yes_focused);
    }

    #[test]
    fn confirm_resolve_yes_queues_kill() {
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals.selected = 0;
        app.terminals_kill_selected();
        let rejection = app.confirm_resolve(true);
        assert!(rejection.is_none());
        let kill = drained_kill_window(&mut app).unwrap();
        assert_eq!(kill.index, 0);
    }

    #[test]
    fn confirm_resolve_no_skips_kill() {
        let mut app = App::new(cfg());
        app.terminals_set_windows(vec![crate::tui::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        app.terminals.selected = 0;
        app.terminals_kill_selected();
        app.confirm_resolve(false);
        assert!(drained_kill_window(&mut app).is_none());
    }

    #[test]
    fn terminals_kill_on_sentinel_is_noop() {
        let mut app = App::new(cfg());
        // Only sentinel in the list.
        app.terminals.selected = app.terminals_rows().len() - 1;
        app.terminals_kill_selected();
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.confirm_dialog().is_none());
    }

    #[test]
    fn diff_line_kind_classifies_known_prefixes() {
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
        let req = drained_attach(&mut app).unwrap();
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
            crate::config::parse_str(
                r#"
                [runtime]
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
        assert!(drained_attach(&mut app).is_none());
    }

    #[test]
    fn queue_service_attach_rejects_custom_service() {
        let cfg = Arc::new(
            crate::config::parse_str(
                r#"
                [runtime]
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

    fn df(path: &str, status: DiffStatus) -> DiffFile {
        DiffFile {
            path: path.into(),
            status,
            additions: 0,
            deletions: 0,
            binary: false,
            old_path: None,
        }
    }

    #[test]
    fn diff_select_clamps_at_bounds() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![
            df("a", DiffStatus::Modified),
            df("b", DiffStatus::Added),
        ]);
        app.diff_mut().select_next();
        assert_eq!(app.diff().selected, 1);
        app.diff_mut().select_next();
        assert_eq!(app.diff().selected, 1);
        app.diff_mut().select_prev();
        assert_eq!(app.diff().selected, 0);
        app.diff_mut().select_prev();
        assert_eq!(app.diff().selected, 0);
    }

    #[test]
    fn select_at_clamps_to_last_item() {
        let mut app = App::new(cfg());
        let last = app.items().len() - 1;
        app.select_at(usize::MAX);
        assert_eq!(app.selected_index(), last);
        app.select_at(0);
        assert_eq!(app.selected_index(), 0);
    }

    #[test]
    fn diff_select_at_clamps() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![
            df("a", DiffStatus::Modified),
            df("b", DiffStatus::Added),
        ]);
        app.diff_mut().select_at(usize::MAX);
        assert_eq!(app.diff().selected, 1);
        app.diff_mut().select_at(0);
        assert_eq!(app.diff().selected, 0);
    }

    #[test]
    fn terminals_select_at_clamps() {
        let mut app = App::new(cfg());
        // Default state has at least the `+ new shell` sentinel row,
        // so `terminals_rows().len()` is non-zero.
        let last = app.terminals_rows().len() - 1;
        app.terminals_select_at(usize::MAX);
        assert_eq!(app.terminals().selected, last);
        app.terminals_select_at(0);
        assert_eq!(app.terminals().selected, 0);
    }

    #[test]
    fn diff_body_h_scroll_by_clamps_at_zero() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        // Seed a cache with a long content line so the upper-bound
        // clamp leaves plenty of room for the test to drive the
        // value past 12 columns. Without lines, max_h_scroll = 0.
        app.diff.body_width.set(20);
        app.diff_mut().set_cache(
            "a".into(),
            vec![DiffLine {
                kind: DiffLineKind::Context,
                text: format!(" {}", "x".repeat(200)),
                old_lineno: Some(1),
                new_lineno: Some(1),
                spans: vec![],
            }],
        );
        assert_eq!(app.diff().body_h_scroll(), 0);
        // Negative delta on an already-zero offset stays at zero —
        // can't pan past the left edge.
        app.diff_mut().body_h_scroll_by(-5);
        assert_eq!(app.diff().body_h_scroll(), 0);
        app.diff_mut().body_h_scroll_by(12);
        assert_eq!(app.diff().body_h_scroll(), 12);
        app.diff_mut().body_h_scroll_by(-20);
        assert_eq!(app.diff().body_h_scroll(), 0);
    }

    #[test]
    fn diff_body_h_scroll_clamps_at_longest_line() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        // Viewport = 20 cols, content = 30 chars, gutter formula
        // adds 2*1 + 4 = 6 → rendered width = 36 → max_scroll = 16.
        app.diff.body_width.set(20);
        app.diff_mut().set_cache(
            "a".into(),
            vec![DiffLine {
                kind: DiffLineKind::Context,
                text: format!(" {}", "x".repeat(30)),
                old_lineno: Some(1),
                new_lineno: Some(1),
                spans: vec![],
            }],
        );
        // Slam to the right — far past the longest line.
        app.diff_mut().body_h_scroll_by(9999);
        assert_eq!(app.diff().body_h_scroll(), 16);
        // No further movement once at the cap.
        app.diff_mut().body_h_scroll_by(50);
        assert_eq!(app.diff().body_h_scroll(), 16);
        // Shrinking the viewport raises the cap; growing it lowers
        // the cap, and the *read* side clamps any stale stored value.
        app.diff.body_width.set(100);
        assert_eq!(app.diff().body_h_scroll(), 0);
    }

    #[test]
    fn diff_toggle_wrap_on_clears_h_scroll_for_active_file() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        // Seed a wide line + tight viewport so the upper-bound clamp
        // leaves enough headroom for the test to pan to 16.
        app.diff.body_width.set(20);
        app.diff_mut().set_cache(
            "a".into(),
            vec![DiffLine {
                kind: DiffLineKind::Context,
                text: format!(" {}", "x".repeat(100)),
                old_lineno: Some(1),
                new_lineno: Some(1),
                spans: vec![],
            }],
        );
        // Wrap starts off; pan right a few notches.
        assert!(!app.diff().wrap);
        app.diff_mut().body_h_scroll_by(16);
        assert_eq!(app.diff().body_h_scroll(), 16);
        // Turning wrap on must drop the offset so the wrapped view
        // can't render with a non-zero h-scroll (which would chop
        // the first N columns of every wrapped row).
        app.diff_mut().toggle_wrap();
        assert!(app.diff().wrap);
        assert_eq!(app.diff().body_h_scroll(), 0);
        // Toggling wrap off again starts fresh at 0 (we don't bother
        // restoring the pre-toggle offset — the user can re-pan).
        app.diff_mut().toggle_wrap();
        assert!(!app.diff().wrap);
        assert_eq!(app.diff().body_h_scroll(), 0);
    }

    #[test]
    fn diff_mark_stale_clears_cache() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        app.diff_mut().set_cache("a".into(), vec![]);
        assert!(app.diff().cache_for("a").is_some());
        app.diff_mut().mark_stale();
        assert!(app.diff().cache_for("a").is_none());
        assert!(!app.diff().loaded);
    }

    #[test]
    fn diff_focus_toggles() {
        let mut app = App::new(cfg());
        assert_eq!(app.diff().focus(), DiffFocus::Files);
        app.diff_mut().toggle_focus();
        assert_eq!(app.diff().focus(), DiffFocus::Body);
        app.diff_mut().toggle_focus();
        assert_eq!(app.diff().focus(), DiffFocus::Files);
    }

    #[test]
    fn diff_jump_hunk_skips_to_next_at_marker() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        // Tight viewport so scroll clamping (which keeps the last
        // visible line at the bottom) doesn't pin the offset to 0.
        app.diff.body_height.set(2);
        app.diff_mut().set_cache(
            "a".into(),
            vec![
                DiffLine {
                    kind: DiffLineKind::Header,
                    text: "diff --git a/a b/a".into(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Hunk,
                    text: "@@ -1,3 +1,3 @@".into(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: " ctx".into(),
                    old_lineno: Some(1),
                    new_lineno: Some(1),
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Hunk,
                    text: "@@ -10,3 +10,3 @@".into(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: " ctx2".into(),
                    old_lineno: Some(10),
                    new_lineno: Some(10),
                    spans: vec![],
                },
            ],
        );
        app.diff_mut().jump_hunk_next();
        assert_eq!(app.diff().body_scroll(), 1);
        app.diff_mut().jump_hunk_next();
        assert_eq!(app.diff().body_scroll(), 3);
        app.diff_mut().jump_hunk_next();
        // No further hunk — stays put.
        assert_eq!(app.diff().body_scroll(), 3);
        app.diff_mut().jump_hunk_prev();
        assert_eq!(app.diff().body_scroll(), 1);
        app.diff_mut().jump_hunk_prev();
        assert_eq!(app.diff().body_scroll(), 1);
    }

    #[test]
    fn body_mode_toggles_diff_and_read() {
        let mut app = App::new(cfg());
        assert_eq!(app.diff().body_mode(), BodyMode::Diff);
        app.diff_mut().toggle_body_mode();
        assert_eq!(app.diff().body_mode(), BodyMode::Read);
        app.diff_mut().toggle_body_mode();
        assert_eq!(app.diff().body_mode(), BodyMode::Diff);
    }

    #[test]
    fn diff_mark_stale_clears_read_cache() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        app.diff_mut().set_read_cache(
            "a".into(),
            vec![ReadLine {
                kind: ReadLineKind::Plain,
                lineno: 1,
                text: "x".into(),
                spans: vec![],
            }],
        );
        assert!(app.diff().read_cache_for("a").is_some());
        app.diff_mut().mark_stale();
        assert!(app.diff().read_cache_for("a").is_none());
    }

    #[test]
    fn scroll_routes_to_active_mode() {
        let mut app = App::new(cfg());
        app.diff_mut().set_files(vec![df("a", DiffStatus::Modified)]);
        // Diff-mode content + tight viewport so scroll is unclamped.
        app.diff.body_height.set(1);
        app.diff_mut().set_cache(
            "a".into(),
            vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: " a".into(),
                    old_lineno: Some(1),
                    new_lineno: Some(1),
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: " b".into(),
                    old_lineno: Some(2),
                    new_lineno: Some(2),
                    spans: vec![],
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: " c".into(),
                    old_lineno: Some(3),
                    new_lineno: Some(3),
                    spans: vec![],
                },
            ],
        );
        // Read-mode content with a different length — proves the
        // bottom clamp is mode-aware too.
        let read_lines: Vec<ReadLine> = (1..=5)
            .map(|i| ReadLine {
                kind: ReadLineKind::Plain,
                lineno: i,
                text: format!("line{i}"),
                spans: vec![],
            })
            .collect();
        app.diff_mut().set_read_cache("a".into(), read_lines);

        use crate::tui::shared::scroll::Axis;

        // Scroll in diff mode, confirm only diff_scroll changes.
        app.diff_mut().body_scroll_by(2);
        assert_eq!(app.diff().body_scroll(), 2);
        assert_eq!(app.diff().diff_scroll.get("a", Axis::Vertical), 2);
        assert_eq!(app.diff().read_scroll.get("a", Axis::Vertical), 0);

        // Switch to read mode; scroll reads from the read map (empty).
        app.diff_mut().toggle_body_mode();
        assert_eq!(app.diff().body_mode(), BodyMode::Read);
        assert_eq!(app.diff().body_scroll(), 0);

        // Scroll in read mode; only read_scroll changes.
        app.diff_mut().body_scroll_by(3);
        assert_eq!(app.diff().body_scroll(), 3);
        assert_eq!(app.diff().read_scroll.get("a", Axis::Vertical), 3);
        assert_eq!(app.diff().diff_scroll.get("a", Axis::Vertical), 2);

        // Switch back — diff scroll is preserved.
        app.diff_mut().toggle_body_mode();
        assert_eq!(app.diff().body_scroll(), 2);
    }

    #[test]
    fn diff_g_chord_fires_on_second_press() {
        let mut app = App::new(cfg());
        // First press arms the chord.
        assert!(!app.diff_mut().consume_g_chord());
        // Second press within the window fires.
        assert!(app.diff_mut().consume_g_chord());
        // After firing the state resets — next first press doesn't fire.
        assert!(!app.diff_mut().consume_g_chord());
    }

    #[test]
    fn switcher_form_focus_toggles_between_path_and_branch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.open_create_form(Vec::new(), None);
        let initial = app.switcher().unwrap().creating.as_ref().unwrap().focus;
        app.switcher_form_toggle_focus();
        let toggled = app.switcher().unwrap().creating.as_ref().unwrap().focus;
        assert_ne!(initial, toggled);
    }

    /// Branch field auto-syncs the path as `<parent>/<slug(branch)>`
    /// while the user hasn't manually edited the path. Slugifier
    /// drops slashes, so `feat/auth` → `feat-auth`.
    #[test]
    fn switcher_form_path_auto_syncs_from_branch() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        // Hop to "+ new worktree" sentinel + open the form.
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.open_create_form(Vec::new(), None);

        // Type "feat/auth" → path follows along.
        for c in "feat/auth".chars() {
            app.switcher_form_push_char(c);
        }
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.branch_input, "feat/auth");
        assert!(
            form.path_input.ends_with("/feat-auth"),
            "{}",
            form.path_input
        );
        assert!(!form.path_dirty);

        // Tab + edit path → path stops following the branch.
        app.switcher_form_toggle_focus();
        app.switcher_form_push_char('/');
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert!(form.path_dirty);
        let dirty_path = form.path_input.clone();

        // Tab back, type more in the branch — path stays put.
        app.switcher_form_toggle_focus();
        app.switcher_form_push_char('-');
        app.switcher_form_push_char('v');
        app.switcher_form_push_char('2');
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.branch_input, "feat/auth-v2");
        assert_eq!(form.path_input, dirty_path);
    }

    /// Up/Down arrows must move selection within the form's branch
    /// list when focus is on the branch field. (Also covers
    /// clamping at both ends.)
    #[test]
    fn switcher_form_arrow_navigation_moves_selection() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        app.open_create_form(
            vec![
                crate::runtime::BranchEntry {
                    name: "main".into(),
                    remote_only: false,
                },
                crate::runtime::BranchEntry {
                    name: "feat-x".into(),
                    remote_only: false,
                },
                crate::runtime::BranchEntry {
                    name: "feat-y".into(),
                    remote_only: false,
                },
            ],
            None,
        );
        let sel = |a: &App| a.switcher().unwrap().creating.as_ref().unwrap().selected;
        assert_eq!(sel(&app), 0);
        app.switcher_form_select_next();
        assert_eq!(sel(&app), 1);
        app.switcher_form_select_next();
        assert_eq!(sel(&app), 2);
        // Clamp at end.
        app.switcher_form_select_next();
        assert_eq!(sel(&app), 2);
        app.switcher_form_select_prev();
        assert_eq!(sel(&app), 1);
    }

    /// Filter narrows the displayed branches as the user types;
    /// the create-new sentinel appears only when no branch matches
    /// the input exactly.
    #[test]
    fn switcher_form_filter_and_sentinel() {
        let mut app = App::new(cfg());
        app.open_worktree_switcher(rows());
        app.switcher_select_next();
        app.switcher_select_next();
        app.switcher_confirm();
        let branches = vec![
            crate::runtime::BranchEntry {
                name: "main".into(),
                remote_only: false,
            },
            crate::runtime::BranchEntry {
                name: "feat-x".into(),
                remote_only: false,
            },
            crate::runtime::BranchEntry {
                name: "feat-y".into(),
                remote_only: false,
            },
        ];
        app.open_create_form(branches, None);

        // Empty input → all branches visible, no sentinel.
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.filtered.len(), 3);
        assert!(!form.show_create_sentinel());

        // Type "feat" → 2 matches, no exact branch named "feat" →
        // sentinel appears.
        for c in "feat".chars() {
            app.switcher_form_push_char(c);
        }
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.filtered.len(), 2);
        assert!(form.show_create_sentinel());
        assert_eq!(form.total_options(), 3);

        // Type "-x" so the input becomes "feat-x" — exact match
        // suppresses the sentinel.
        app.switcher_form_push_char('-');
        app.switcher_form_push_char('x');
        let form = app.switcher().unwrap().creating.as_ref().unwrap();
        assert_eq!(form.filtered.len(), 1);
        assert!(!form.show_create_sentinel());
    }
}
