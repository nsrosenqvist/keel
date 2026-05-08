//! TUI application state.
//!
//! The model and the controller. Pure functions — no terminal I/O here.

use crate::palette::Palette;
use crate::runner::RunState;
use crate::services::ServicePane;
use scaffl_config::{Config, Recipe, ScriptCommand, model::UiPane};
use scaffl_container::Backend;
use scaffl_runtime::Executor;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Service,
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

    pub async fn poll_run(&mut self) {
        if let Some(run) = self.current_run.as_mut() {
            run.poll_completion().await;
        }
    }

    /// Drain output from every service pane. Cheap when nothing has
    /// arrived; called on every pre-render hook.
    pub fn drain_services(&mut self) {
        for pane in self.services.values_mut() {
            pane.drain();
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
}

fn build_items(config: &Config) -> Vec<Item> {
    let mut items = Vec::new();
    for pane in &config.ui.panes {
        if let UiPane::Service { service, .. } = pane {
            items.push(Item {
                name: service.clone(),
                kind: ItemKind::Service,
            });
        }
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
