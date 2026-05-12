//! Per-`ItemKind` dispatch as trait objects.
//!
//! Before this lived here, two pieces of per-kind logic (the
//! sidebar indicator-style colour and the right-pane output
//! renderer) sat as `match item.kind { … }` blocks in `ui.rs`.
//! Adding a sixth `ItemKind` meant remembering both call sites.
//!
//! Now each kind has a ZST struct implementing [`SidebarItem`],
//! and [`for_kind`] hands back a `&'static` trait object. Per-kind
//! logic lives in one place per kind (the `impl` block); the
//! shared call sites in `ui.rs` (`item_indicator_style` /
//! `render_output_for_item`) become thin dispatchers.
//!
//! The structs are ZSTs because the kind alone is enough to pick
//! the right behavior — the per-row data (`name`) is still passed
//! in by the caller via `&str`. Recipe and Script keep their kind
//! statically so they can pass the right discriminator into
//! `app.run_for(kind, name)`.

use crate::tui::app::App;
use crate::tui::ui::{
    render_idle_output, render_run_buffer, render_service_logs, render_watcher,
    run_indicator_style, service_indicator_style, watcher_indicator_style,
};
use crate::tui::views::control_center::state::ItemKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Per-kind behavior for a single sidebar row. ZST implementors;
/// callers pass `(name, app)` so the methods stay borrow-flexible.
pub trait SidebarItem {
    /// Style for the sidebar bullet glyph.
    fn indicator_style(&self, name: &str, app: &App) -> Style;
    /// Render the right-pane output area for the selected row.
    fn render_output(&self, name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect);
}

pub struct RuntimeItem;
pub struct ServiceItem;
pub struct WatcherItem;
pub struct RecipeItem;
pub struct ScriptItem;

impl SidebarItem for RuntimeItem {
    fn indicator_style(&self, _name: &str, app: &App) -> Style {
        run_indicator_style(app.lifecycle_run())
    }
    fn render_output(&self, _name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect) {
        match app.lifecycle_run() {
            Some(run) => render_run_buffer(run, accent, frame, area),
            None => render_idle_output(
                accent,
                frame,
                area,
                "no lifecycle action has run yet",
                Some("U up all · D down all · R restart all · S stop all"),
            ),
        }
    }
}

impl SidebarItem for ServiceItem {
    fn indicator_style(&self, name: &str, app: &App) -> Style {
        service_indicator_style(app, name)
    }
    fn render_output(&self, _name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect) {
        if let Some(service) = app.selected_service() {
            render_service_logs(service, accent, frame, area);
        }
    }
}

impl SidebarItem for WatcherItem {
    fn indicator_style(&self, name: &str, app: &App) -> Style {
        watcher_indicator_style(app, name)
    }
    fn render_output(&self, _name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect) {
        if let Some(watcher) = app.selected_watcher() {
            render_watcher(watcher, accent, frame, area);
        }
    }
}

impl SidebarItem for RecipeItem {
    fn indicator_style(&self, name: &str, app: &App) -> Style {
        run_indicator_style(app.run_for(ItemKind::Recipe, name))
    }
    fn render_output(&self, _name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect) {
        render_command_output(app, accent, frame, area);
    }
}

impl SidebarItem for ScriptItem {
    fn indicator_style(&self, name: &str, app: &App) -> Style {
        run_indicator_style(app.run_for(ItemKind::Script, name))
    }
    fn render_output(&self, _name: &str, app: &App, accent: Color, frame: &mut Frame, area: Rect) {
        render_command_output(app, accent, frame, area);
    }
}

/// Shared render path for Recipe + Script — both look at the
/// selected row's run state and fall through to an "idle" panel
/// when no run has happened this session.
fn render_command_output(app: &App, accent: Color, frame: &mut Frame, area: Rect) {
    match app.selected_run() {
        Some(run) => render_run_buffer(run, accent, frame, area),
        None => render_idle_output(accent, frame, area, "press enter to run", None),
    }
}

/// Hand back the `&'static dyn SidebarItem` for `kind`. Adding a
/// sixth `ItemKind` = add a struct + impl above, add one match arm
/// here, done.
pub fn for_kind(kind: ItemKind) -> &'static dyn SidebarItem {
    match kind {
        ItemKind::Runtime => &RuntimeItem,
        ItemKind::Service => &ServiceItem,
        ItemKind::Watcher => &WatcherItem,
        ItemKind::Recipe => &RecipeItem,
        ItemKind::Script => &ScriptItem,
    }
}
