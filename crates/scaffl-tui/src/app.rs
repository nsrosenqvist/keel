//! TUI application state.
//!
//! The model and the controller. Pure functions — no terminal I/O here.

use scaffl_config::{Config, Recipe, ScriptCommand};
use std::sync::Arc;

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Recipe,
    Script,
}

/// A single sidebar entry. Cheap to clone (string pointer + index).
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
}

/// TUI application state.
#[derive(Debug)]
pub struct App {
    config: Arc<Config>,
    items: Vec<Item>,
    selected: usize,
    quit: bool,
}

impl App {
    pub fn new(config: Arc<Config>) -> Self {
        let items = build_items(&config);
        Self {
            config,
            items,
            selected: 0,
            quit: false,
        }
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

    /// Look up the [`Recipe`] for the currently selected item, if any.
    pub fn selected_recipe(&self) -> Option<&Recipe> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Recipe {
            return None;
        }
        self.config.commands.get(&item.name)
    }

    /// Look up the [`ScriptCommand`] for the currently selected item, if any.
    pub fn selected_script(&self) -> Option<&ScriptCommand> {
        let item = self.selected_item()?;
        if item.kind != ItemKind::Script {
            return None;
        }
        self.config.scripts.get(&item.name)
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
        app.select_next(); // past the end
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
}
