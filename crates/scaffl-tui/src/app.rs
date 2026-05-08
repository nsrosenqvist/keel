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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

/// TUI application state.
pub struct App {
    config: Arc<Config>,
    items: Vec<Item>,
    selected: usize,
    quit: bool,
    executor: Option<Executor>,
    backend: Option<Arc<dyn Backend>>,
    current_run: Option<RunState>,
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
            current_run: None,
            services,
            watchers: BTreeMap::new(),
            flash: None,
            mode: Mode::Normal,
            palette: None,
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

    pub fn current_run(&self) -> Option<&RunState> {
        self.current_run.as_ref()
    }

    pub fn current_run_mut(&mut self) -> Option<&mut RunState> {
        self.current_run.as_mut()
    }

    /// Try to launch the currently selected item. Returns [`LaunchRejection`]
    /// when the launch can't proceed; the caller renders the reason as a
    /// flash message.
    pub fn try_launch_selected(&mut self) -> Result<(), LaunchRejection> {
        if self.current_run.as_ref().is_some_and(|r| !r.is_done()) {
            return Err(LaunchRejection::AlreadyRunning);
        }
        let executor = self
            .executor
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();

        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();

        match item.kind {
            ItemKind::Service => {
                return Err(LaunchRejection::NotRunnable(
                    "service panes show logs; press q to quit or navigate to a recipe".into(),
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
        self.current_run = Some(run);
        self.flash = None;
        Ok(())
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

    pub fn drain_run(&mut self) {
        if let Some(run) = self.current_run.as_mut() {
            run.drain();
        }
    }

    /// Abort the current run, if any is in flight. Returns true when an
    /// abort was issued (so the caller can flash a status message).
    pub fn abort_current_run(&mut self) -> bool {
        match self.current_run.as_mut() {
            Some(run) if !run.is_done() => {
                run.abort();
                true
            }
            _ => false,
        }
    }

    /// Run a backend lifecycle action (up / down / stop / restart)
    /// against the given services. An empty slice means "all services".
    /// The Child is wrapped as a [`RunState`] so output streams into
    /// the right pane like a recipe run.
    ///
    /// Returns `Err` describing the rejection when no backend is
    /// configured or a run is already in flight.
    pub async fn run_service_action(
        &mut self,
        action: &'static str,
        services: &[&str],
    ) -> Result<(), LaunchRejection> {
        if self.current_run.as_ref().is_some_and(|r| !r.is_done()) {
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
                self.current_run = Some(RunState::spawn_child(label, child));
                Ok(())
            }
            Err(e) => Err(LaunchRejection::NotRunnable(format!("{e}"))),
        }
    }

    pub async fn poll_run(&mut self) {
        if let Some(run) = self.current_run.as_mut() {
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
