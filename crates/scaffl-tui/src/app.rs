//! TUI application state.
//!
//! The model and the controller. Pure functions — no terminal I/O here.

use crate::runner::RunState;
use scaffl_config::{Config, Recipe, ScriptCommand};
use scaffl_runtime::Executor;
use std::sync::Arc;

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
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

/// TUI application state.
pub struct App {
    config: Arc<Config>,
    items: Vec<Item>,
    selected: usize,
    quit: bool,
    executor: Option<Executor>,
    current_run: Option<RunState>,
    /// Last rejection / status banner (decays after a few seconds — kept
    /// simple by just clearing on the next successful action).
    pub flash: Option<String>,
}

impl App {
    pub fn new(config: Arc<Config>) -> Self {
        let items = build_items(&config);
        Self {
            config,
            items,
            selected: 0,
            quit: false,
            executor: None,
            current_run: None,
            flash: None,
        }
    }

    pub fn with_executor(mut self, executor: Executor) -> Self {
        self.executor = Some(executor);
        self
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
}

fn build_items(config: &Config) -> Vec<Item> {
    let mut items = Vec::with_capacity(config.commands.len() + config.scripts.len());
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
    fn build_items_sorts_recipes_before_scripts() {
        let cfg = cfg();
        let app = App::new(cfg);
        assert_eq!(app.items().len(), 2);
        assert!(app.items().iter().all(|i| i.kind == ItemKind::Recipe));
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
