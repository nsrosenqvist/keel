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
}

impl App {
    pub fn new(config: Arc<Config>) -> Self {
        let items = build_items(&config);
        let services = collect_service_panes(&config);
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
        }
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
