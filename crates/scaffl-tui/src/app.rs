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
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
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
        }
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

    /// Try to launch the currently selected item. Returns [`LaunchRejection`]
    /// when the launch can't proceed; the caller renders the reason as a
    /// flash message. If the selected item is already running, returns
    /// [`LaunchRejection::AlreadyRunning`] — the caller is expected to
    /// open the kill-and-restart confirmation modal in that case.
    pub fn try_launch_selected(&mut self) -> Result<(), LaunchRejection> {
        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();

        // Per-row "is this already running?" gate replaces the old
        // single-slot `current_run` check. Different rows can run
        // concurrently; only the same row collides with itself.
        let key: RunKey = (item.kind, item.name.clone());
        if self
            .runs
            .get(&key)
            .is_some_and(|r| !r.is_done())
        {
            return Err(LaunchRejection::AlreadyRunning);
        }

        let executor = self
            .executor
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();

        match item.kind {
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

        let run = RunState::spawn(&executor, item.name, Vec::new());
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

    /// Move the sidebar selection to the palette's current match and close
    /// the palette. Returns true iff there was a match to confirm.
    pub fn confirm_palette(&mut self) -> bool {
        let Some(palette) = self.palette.as_ref() else {
            return false;
        };
        let Some(m) = palette.selected_match() else {
            return false;
        };
        self.selected = m.item_index;
        self.close_palette();
        true
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
                if let Some(idx) = self.items.iter().position(|i| i.kind == key.0 && i.name == key.1)
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

/// Reconstruct the sidebar item list from live state. The order is
/// stable: services (declared first in scaffl.toml order, then any
/// auto-discovered ones), watchers, recipes, scripts.
fn build_items_from(
    config: &Config,
    services: &BTreeMap<String, ServicePane>,
    watchers: &BTreeMap<String, WatcherPane>,
) -> Vec<Item> {
    let mut items = Vec::new();
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

    fn cfg() -> Arc<Config> {
        Arc::new(
            scaffl_config::parse_str(
                r#"
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
    fn empty_config_has_no_items() {
        let cfg = Arc::new(scaffl_config::Config::default());
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
}
