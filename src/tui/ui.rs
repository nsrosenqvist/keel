//! Rendering — pure function from [`App`] state to a ratatui [`Frame`].
//!
//! Layout is three vertical bands plus an optional palette overlay:
//!
//! ```text
//! ┌─ top status bar ───────────────────────────────────────────────┐
//! │ keel · <project> · worktree:<slug> · offset:<n>              │
//! ├─ services ──────────────┬─ <selected item details / output> ────┤
//! │ ● app    ●              │                                       │
//! │ ● worker ●              │                                       │
//! ├─ watchers ──────────────┤                                       │
//! │ ◇ watch:test            │                                       │
//! ├─ recipes ───────────────┤                                       │
//! │ ▸ greet                 │                                       │
//! │ ▸ stream                │                                       │
//! ├─ scripts ───────────────┤                                       │
//! │ ▪ seed                  │                                       │
//! ├─ hotkey hint bar ───────────────────────────────────────────────┤
//! │ ↑↓ nav · enter run · s stop · / palette · q quit               │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use crate::container::ServiceStatus;
use crate::runtime::OutputStream;
use crate::tui::app::{App, Mode};
use crate::tui::runner::{CapturedLine, RunState};
use crate::tui::services::ServicePane;
use crate::tui::views::control_center::state::{Item, ItemKind};
use crate::tui::views::diff::state::{BodyMode, DiffFocus};
use crate::tui::watchers::{WatcherPane, WatcherState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, ListItem, Padding, Paragraph, Wrap},
};

pub(crate) const SIDEBAR_RATIO: u16 = 28;
const TOP_BAR_HEIGHT: u16 = 1;
const STATUS_BAR_HEIGHT: u16 = 1;

/// Per-view accent. Each top-level view gets its own muted hue so
/// users have a visual cue for which view they're in without
/// reading the status-bar tag — peach for the home/control center,
/// mint teal for the tmux-backed terminals view, soft sky blue for
/// the diff view. Indexed colors so terminals with non-standard
/// 16-color palettes still land somewhere reasonable.
fn view_accent(view: crate::tui::app::View) -> Color {
    match view {
        crate::tui::app::View::ControlCenter => Color::Indexed(215),
        crate::tui::app::View::Terminals => Color::Indexed(79),
        // Saturated sky blue — same hue family as the original
        // 110 so the diff view still "reads" as blue, but bright
        // enough to sit next to the peach (215) and mint teal (79)
        // accents without disappearing into the dark-grey unfocused
        // panel borders.
        crate::tui::app::View::Diff => Color::Indexed(117),
    }
}

pub(crate) fn accent_of(app: &App) -> Color {
    view_accent(app.view())
}

/// Selection highlight chrome — same across all views so the
/// "selected row" affordance reads consistently. Subtle dim grey
/// background pairs with the active accent for the row's text;
/// avoids the loud cyan-bg / black-fg contrast we used before.
pub(crate) const SELECTION_BG: Color = Color::Indexed(238);
pub(crate) const SELECTION_FG: Color = Color::Indexed(255);

pub fn render(app: &App, frame: &mut Frame) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(TOP_BAR_HEIGHT),
            Constraint::Min(1),
            Constraint::Length(STATUS_BAR_HEIGHT),
        ])
        .split(frame.area());

    render_top_bar(app, frame, outer[0]);

    // Body delegated by view. Each view owns its sidebar + right
    // pane; only the chrome (top bar, status bar) is shared.
    // Clear stale diff pane rects when leaving the diff view so a
    // mouse click in another view can't accidentally hit a rect
    // populated by a previous frame.
    if app.view() != crate::tui::app::View::Diff {
        app.diff().files_rect.set(None);
        app.diff().body_rect.set(None);
    }
    match app.view() {
        crate::tui::app::View::ControlCenter => render_control_center(app, frame, outer[1]),
        crate::tui::app::View::Terminals => {
            crate::tui::views::terminals::view::render(app, frame, outer[1])
        }
        crate::tui::app::View::Diff => crate::tui::views::diff::view::render(app, frame, outer[1]),
    }

    render_status(app, frame, outer[2]);

    let accent = accent_of(app);
    if app.mode() == Mode::Palette
        && let Some(palette) = app.palette()
    {
        crate::tui::dialogs::palette_view::render(app, palette, accent, frame);
    }
    if app.mode() == Mode::Confirm
        && let Some(dialog) = app.confirm_dialog()
    {
        crate::tui::dialogs::confirm_view::render(app, dialog, accent, frame);
    }
    if app.mode() == Mode::ArgsPrompt
        && let Some(prompt) = app.args_prompt()
    {
        crate::tui::dialogs::args_prompt_view::render(prompt, accent, frame);
    }
    if app.mode() == Mode::WorktreeSwitcher
        && let Some(switcher) = app.switcher()
    {
        crate::tui::dialogs::switcher_view::render(switcher, accent, frame);
    }
}

/// Control-center body: sidebar takes the full left column; the
/// right column is split vertically into an info panel (kv pairs +
/// description) on top and an output panel underneath. The legacy
/// bottom-left "details" panel is gone — the same data now lives
/// above the output for the selected item.
fn render_control_center(app: &App, frame: &mut Frame, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);
    render_sidebar(app, frame, body[0]);
    render_right_pane(app, frame, body[1]);
}

// ───────────────────────── top bar ─────────────────────────

fn render_top_bar(app: &App, frame: &mut Frame, area: Rect) {
    let project = app
        .config()
        .project
        .name
        .clone()
        .unwrap_or_else(|| "keel".into());

    // Top bar's "keel" wordmark stays bold white regardless of
    // view — pinning it to the active accent made it visually
    // collide with the first sidebar group's accent-colored title
    // immediately below. The view accent already has plenty of
    // representation (panel titles, status-bar tag, group headers).
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("  keel ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(project, Style::default().add_modifier(Modifier::BOLD)),
    ];

    // Devcontainer indicator. Surfaces *that* the project is opted
    // in (so users aren't surprised when `n` drops them into a
    // container) and *which* container — the deterministic name is
    // a useful breadcrumb when multiple worktrees / projects are
    // running side by side.
    if let Some(dc) = app.devcontainer() {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "▣ devcontainer ",
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled(
            dc.container_name().to_string(),
            Style::default().fg(Color::Gray),
        ));
    }

    // Branch + dirty marker — globally relevant across every view.
    // The branch is set by the CLI from the detected Identity at
    // startup (and on every worktree hot-reload). Dirty count comes
    // from the same `git status --porcelain` we already shell out
    // for the diff view; preloaded on startup so this header is
    // populated even before the user opens the diff view.
    if let Some(branch) = app.branch() {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            branch.to_string(),
            Style::default().fg(Color::Gray),
        ));
        // "vs <trunk>" — surfaces what the diff view is anchored
        // against so users know whether `●3` means "3 working-tree
        // changes" or "3 files differ from main." Hidden when the
        // current branch IS the trunk (showing "main vs main" reads
        // odd) and when no trunk could be detected.
        if let Some(trunk) = app.diff().trunk.as_deref()
            && trunk != branch
        {
            spans.push(Span::styled(" vs ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                trunk.to_string(),
                Style::default().fg(Color::Gray),
            ));
        }
        let dirty = app.diff().files.len();
        if app.diff().loaded && dirty > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("●{dirty}"),
                Style::default().fg(Color::Yellow),
            ));
        } else if app.diff().loaded {
            spans.push(Span::styled(" clean", Style::default().fg(Color::DarkGray)));
        }
    }

    let p = Paragraph::new(Line::from(spans));
    frame.render_widget(p, area);
}

// ─────────────────────── sidebar ───────────────────────

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    use crate::tui::shared::sidebar_layout::{JoinPosition, SidebarGroup, render_grouped_sidebar};

    // Bucket items by kind into the four named groups. Recipes and
    // scripts share a single "commands" group — to most users they're
    // the same thing (a runnable command), and the distinction is
    // still visible where it matters (kind tag in the palette and
    // detail pane). Sidebar order preserves the iteration order of
    // `app.items()` within the commands group, which is recipes
    // (alphabetical) then scripts (alphabetical).
    let mut runtime: Vec<&Item> = Vec::new();
    let mut services: Vec<&Item> = Vec::new();
    let mut watchers: Vec<&Item> = Vec::new();
    let mut commands: Vec<&Item> = Vec::new();
    for item in app.items() {
        match item.kind {
            ItemKind::Runtime => runtime.push(item),
            ItemKind::Service => services.push(item),
            ItemKind::Watcher => watchers.push(item),
            ItemKind::Recipe | ItemKind::Script => commands.push(item),
        }
    }
    let mut buckets: Vec<(&'static str, Vec<&Item>)> = Vec::new();
    if !runtime.is_empty() {
        buckets.push(("runtime", runtime));
    }
    if !services.is_empty() {
        buckets.push(("services", services));
    }
    if !watchers.is_empty() {
        buckets.push(("watchers", watchers));
    }
    if !commands.is_empty() {
        buckets.push(("commands", commands));
    }

    if buckets.is_empty() {
        let block = panel_block(" commands ", accent_of(app));
        let body = Paragraph::new(Line::from(Span::styled(
            "  (no items)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(body, area);
        return;
    }

    // Build shared SidebarGroups with pre-rendered list items + the
    // local-selected index within each bucket. The flat-index order
    // matches `app.items()` (build_items is the canonical order), so
    // the global selection maps to one bucket's local index.
    let global_idx = app.selected_index();
    let mut groups: Vec<SidebarGroup<'_>> = Vec::with_capacity(buckets.len());
    let mut cursor = 0usize;
    for (label, items) in &buckets {
        let local_selected = if global_idx >= cursor && global_idx < cursor + items.len() {
            Some(global_idx - cursor)
        } else {
            None
        };
        let list_items: Vec<ListItem> = items
            .iter()
            .map(|item| ListItem::new(sidebar_item_line(app, item)))
            .collect();
        groups.push(SidebarGroup {
            label,
            items: list_items,
            selected_local: local_selected,
        });
        cursor += items.len();
    }

    // Mark adjacent runtime + services groups so they render as one
    // joined block (shared horizontal seam) — they're related enough
    // (lifecycle of the workspace's runtime processes) to read as one
    // grouped panel rather than two separate ones.
    let mut joins = vec![JoinPosition::Standalone; groups.len()];
    for i in 0..groups.len().saturating_sub(1) {
        if groups[i].label == "runtime" && groups[i + 1].label == "services" {
            joins[i] = JoinPosition::Top;
            joins[i + 1] = JoinPosition::Bottom;
        }
    }

    let mut rects = app.sidebar_item_rects.borrow_mut();
    render_grouped_sidebar(
        frame,
        area,
        &groups,
        &joins,
        accent_of(app),
        &mut rects,
        Some("▶ "),
    );
}

/// One control-center sidebar row: kind glyph + name + optional
/// backend badge for non-compose services.
fn sidebar_item_line<'a>(app: &App, item: &'a Item) -> Line<'a> {
    let glyph = item.kind.glyph();
    let glyph_style = item_indicator_style(app, item);
    let mut spans = vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::raw(item.name.clone()),
    ];
    if let Some(badge) = service_backend_badge(app, item) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            badge,
            Style::default().fg(Color::DarkGray).dim(),
        ));
    }
    Line::from(spans)
}

/// Returns a tiny tag for service rows whose backend isn't compose,
/// so the user can see at a glance which lifecycle each service
/// belongs to. Compose services get no badge — they're the default
/// and visual noise on the most common case isn't worth it.
fn service_backend_badge(app: &App, item: &Item) -> Option<&'static str> {
    if item.kind != ItemKind::Service {
        return None;
    }
    let cfg = app.config();
    if cfg.services.systemd.iter().any(|s| s.name == item.name) {
        return Some("systemd");
    }
    if cfg.services.custom.iter().any(|s| s.name == item.name) {
        return Some("custom");
    }
    None
}

fn item_indicator_style(app: &App, item: &Item) -> Style {
    crate::tui::views::control_center::dispatch::for_kind(item.kind)
        .indicator_style(&item.name, app)
}

/// Standard run-state palette for sidebar glyphs:
///   never run         → dark grey
///   running           → yellow
///   exit 0            → green
///   non-zero / aborted → red
///
/// Used for the container row, recipe rows, and script rows — any
/// sidebar entry whose state is a `RunState`. Services and watchers
/// have their own indicator paths because their lifecycle isn't a
/// `RunState` (services tail logs; watchers debounce).
pub(crate) fn run_indicator_style(run: Option<&RunState>) -> Style {
    match run {
        None => Style::default().fg(Color::DarkGray),
        Some(r) if !r.is_done() => Style::default().fg(Color::Yellow),
        Some(r) => match r.exit_code {
            Some(0) => Style::default().fg(Color::Green),
            _ => Style::default().fg(Color::Red),
        },
    }
}

// ─────────────────────── right pane ───────────────────────
//
// Two stacked panels: a small "info" panel up top with the kv
// pairs + description that used to live in the bottom-left
// details panel, and an "output" panel underneath that hosts the
// run / service / watcher buffers. Padding is horizontal-only —
// vertical breathing room comes from blank rows in the body, not
// from block padding, so we don't waste rows we could be filling.

fn render_right_pane(app: &App, frame: &mut Frame, area: Rect) {
    let accent = accent_of(app);
    let Some(item) = app.selected_item() else {
        // Selection-less state (e.g. an empty config). One panel
        // spanning the whole right column reads cleaner than an
        // empty info-over-output split.
        let block = panel_block(" output ", accent);
        frame.render_widget(block, area);
        return;
    };

    let info_inner_width = info_inner_width(area);
    let info_body = build_info_body(app, item, info_inner_width);
    let [info_area, output_area] = split_info_output(area, info_body.len());
    if info_body.is_empty() {
        // No info to show (e.g. service / container with no
        // metadata) — give the output panel the whole column so we
        // don't render an empty bordered slot.
        render_output_for_item(app, item, accent, frame, area);
        return;
    }
    render_info_panel(item, accent, frame, info_area, info_body);
    render_output_for_item(app, item, accent, frame, output_area);
}

/// Stack two panels vertically: info up top, output below. The info
/// panel sizes to its content (borders + rows) but is capped so the
/// output always gets at least 5 rows — a recipe with twenty env
/// vars shouldn't push the buffer off-screen.
pub(crate) fn split_info_output(area: Rect, info_rows: usize) -> [Rect; 2] {
    let max_info_height = area.height.saturating_sub(5).max(3);
    let info_h = ((info_rows as u16) + 2).min(max_info_height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(info_h), Constraint::Min(1)])
        .split(area);
    [chunks[0], chunks[1]]
}

fn info_inner_width(area: Rect) -> usize {
    // Borders eat 2 cols, horizontal padding eats 4 (2 each side).
    (area.width as usize).saturating_sub(2 + 4)
}

fn render_info_panel(
    item: &Item,
    accent: Color,
    frame: &mut Frame,
    area: Rect,
    body: Vec<Line<'static>>,
) {
    let title = info_panel_title(item, accent);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));
    frame.render_widget(Paragraph::new(body).block(block), area);
}

fn info_panel_title(item: &Item, accent: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            item.name.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            item.kind.label(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
        Span::raw(" "),
    ])
}

fn render_output_for_item(app: &App, item: &Item, accent: Color, frame: &mut Frame, area: Rect) {
    crate::tui::views::control_center::dispatch::for_kind(item.kind)
        .render_output(&item.name, app, accent, frame, area);
}

/// Idle output panel — used by recipes / scripts / lifecycle row
/// when nothing has been run yet this session. Title carries an
/// "idle" badge instead of an exit code or duration.
pub(crate) fn render_idle_output(
    accent: Color,
    frame: &mut Frame,
    area: Rect,
    body_text: &str,
    hint: Option<&str>,
) {
    let title = output_pane_title(OutputStatus::Idle, accent);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));
    let mut lines = vec![Line::from(Span::styled(
        body_text.to_string(),
        Style::default().fg(Color::DarkGray),
    ))];
    if let Some(h) = hint {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            h.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(crate) fn render_run_buffer(run: &RunState, accent: Color, frame: &mut Frame, area: Rect) {
    let title = output_pane_title(OutputStatus::Run(run), accent);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));
    let max_lines = area.height.saturating_sub(2) as usize;
    let total = run.buffer.len();
    let start = total.saturating_sub(max_lines);
    let lines: Vec<Line<'static>> = run
        .buffer
        .iter()
        .skip(start)
        .map(render_captured_line)
        .collect();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ─────────────────────── info panel body ───────────────────────

/// Body for the info panel that sits above the output pane. The
/// panel TITLE carries the item name + kind; the body is the
/// definition list — `desc` (when present) leads as the first row,
/// followed by the static kv pairs (in / path / forward_args / …).
/// Lines render under `Padding::horizontal(2)`, so neither the kv
/// builder nor the desc wrapper add a left gutter themselves.
fn build_info_body(app: &App, item: &Item, wrap_width: usize) -> Vec<Line<'static>> {
    let (desc, kv_lines) = match item.kind {
        ItemKind::Recipe => app
            .selected_recipe()
            .map(|r| (r.desc.clone(), recipe_kv_lines(r)))
            .unwrap_or_default(),
        ItemKind::Script => app
            .selected_script()
            .map(|s| (s.desc.clone(), script_kv_lines(s)))
            .unwrap_or_default(),
        ItemKind::Watcher => app
            .selected_watcher()
            .map(|w| (None, watcher_kv_lines(w)))
            .unwrap_or_default(),
        // Services and containers don't have config-level
        // descriptions or static kv data worth showing here. Title
        // already carries the name + kind; status moves into the
        // output panel header.
        _ => (None, Vec::new()),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(text) = desc {
        lines.extend(kv_wrapped("desc", &text, wrap_width));
    }
    lines.extend(kv_lines);
    lines
}

fn recipe_kv_lines(recipe: &crate::config::Recipe) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(kv("in", recipe.service.as_deref().unwrap_or("host")));
    if recipe.tty {
        out.push(kv("tty", "true"));
    }
    if recipe.forward_args {
        out.push(kv("forward_args", "true"));
    }
    if recipe.parallel {
        out.push(kv("parallel", "true"));
    }
    if !recipe.needs.is_empty() {
        out.push(kv("needs", &recipe.needs.join(", ")));
    }
    for (k, v) in &recipe.env {
        out.push(kv(&format!("env.{k}"), v));
    }
    out
}

fn watcher_kv_lines(watcher: &WatcherPane) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(kv("recipe", &watcher.recipe));
    out.push(kv("globs", &watcher.globs.join(", ")));
    out.push(kv(
        "debounce",
        &format!("{} ms", watcher.debounce.as_millis()),
    ));
    out
}

fn script_kv_lines(script: &crate::config::ScriptCommand) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(kv("in", script.service.as_deref().unwrap_or("host")));
    let path_display = script
        .path
        .file_name()
        .map(|f| format!(".keel/commands/{}", f.to_string_lossy()))
        .unwrap_or_else(|| script.path.display().to_string());
    out.push(kv("path", &path_display));
    if script.tty {
        out.push(kv("tty", "true"));
    }
    if script.forward_args {
        out.push(kv("forward_args", "true"));
    }
    if !script.needs.is_empty() {
        out.push(kv("needs", &script.needs.join(", ")));
    }
    for (k, v) in &script.env {
        out.push(kv(&format!("env.{k}"), v));
    }
    out
}

/// Word-wrap `text` to lines no wider than `width`. Single words
/// longer than `width` (rare in human-written descriptions) get
/// their own line and overflow the bound — we'd rather show them
/// truncated by ratatui at the panel border than break inside a
/// word. Empty input → empty output.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

pub(crate) fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<14}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

/// Same as [`kv`] but wraps long values across multiple rows with a
/// hanging indent so continuation lines align under the value
/// column. Used for `desc`, which is the only kv whose value can
/// realistically run wider than the panel.
fn kv_wrapped(key: &str, value: &str, wrap_width: usize) -> Vec<Line<'static>> {
    const KEY_COL: usize = 14;
    let value_width = wrap_width.saturating_sub(KEY_COL).max(1);
    let chunks = wrap_words(value, value_width);
    if chunks.is_empty() {
        return vec![kv(key, "")];
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let label = if i == 0 {
                format!("{key:<KEY_COL$}")
            } else {
                " ".repeat(KEY_COL)
            };
            Line::from(vec![
                Span::styled(label, Style::default().fg(Color::DarkGray)),
                Span::raw(chunk),
            ])
        })
        .collect()
}

// ─────────────────────── output pane title ───────────────────────

/// Variants the output panel's title bar surfaces. The item name +
/// kind already live in the info panel above; the output title only
/// carries dynamic state — exit code, duration, running indicator,
/// idle/missing/error badges.
enum OutputStatus<'a> {
    Idle,
    Run(&'a RunState),
    Service(&'a ServicePane),
    Watcher(&'a WatcherPane),
}

fn output_pane_title(status: OutputStatus<'_>, accent: Color) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            "output",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ];
    let suffix: Vec<Span<'static>> = match status {
        OutputStatus::Idle => vec![Span::styled(
            "  idle ".to_string(),
            Style::default().fg(Color::DarkGray),
        )],
        OutputStatus::Run(run) => run_status_spans(run),
        OutputStatus::Service(svc) => service_status_spans(svc),
        OutputStatus::Watcher(w) => watcher_status_spans(w, accent),
    };
    spans.extend(suffix);
    Line::from(spans)
}

fn run_status_spans(run: &RunState) -> Vec<Span<'static>> {
    let duration = format_duration(run.duration());
    if let Some(code) = run.exit_code {
        if code == 0 {
            return vec![Span::styled(
                format!("  ✓ exit 0 · {duration} "),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )];
        }
        return vec![Span::styled(
            format!("  ✗ exit {code} · {duration} "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )];
    }
    if let Some(err) = &run.error {
        return vec![Span::styled(
            format!("  ! {err} · {duration} "),
            Style::default().fg(Color::Red),
        )];
    }
    vec![Span::styled(
        format!("  ● running · {duration} "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]
}

/// Compact duration formatter for run-pane titles. Sub-second times
/// land in `0.4s` form; ≥10s uses one decimal less; ≥1m switches to
/// `m m s s` form so titles don't sprawl on long runs.
fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{secs:.0}s")
    } else {
        let total = secs as u64;
        let m = total / 60;
        let s = total % 60;
        format!("{m}m{s:02}s")
    }
}

fn render_captured_line(line: &CapturedLine) -> Line<'static> {
    // Stream tint is the *base* style: stderr stays red when the
    // line carries no ANSI of its own (most of our captures don't,
    // since piping disables color in most CLIs), and SGR resets
    // (`ESC[0m`) inside a line drop back to that base rather than
    // ratatui's default. Programs that do emit color (e.g. invoked
    // with FORCE_COLOR=1, or anything routed through the tmux
    // preview path) get rendered with their own palette on top.
    let base = match line.stream {
        OutputStream::Stdout => Style::default(),
        OutputStream::Stderr => Style::default().fg(Color::Red),
    };
    crate::tui::ansi::ansi_to_line(&line.text, base)
}

pub(crate) fn render_service_logs(
    service: &ServicePane,
    accent: Color,
    frame: &mut Frame,
    area: Rect,
) {
    let title = output_pane_title(OutputStatus::Service(service), accent);

    // Horizontal-only padding — every output pane sits in the same
    // visual frame so all three (run / service / lifecycle) read
    // consistently. Compose's own `<svc-N> |` log prefix lives
    // inside the body.
    let block = panel_block_titled(title).padding(Padding::horizontal(2));

    let max_lines = area.height.saturating_sub(2) as usize;
    let total = service.buffer.len();
    let start = total.saturating_sub(max_lines);
    let lines: Vec<Line<'static>> = service
        .buffer
        .iter()
        .skip(start)
        .map(render_captured_line)
        .collect();

    let body = if service.buffer.is_empty() && service.tail_error.is_none() {
        // Block padding now provides the left gutter; no leading "  "
        // needed in the placeholder.
        vec![Line::from(Span::styled(
            "waiting for output…",
            Style::default().fg(Color::DarkGray),
        ))]
    } else if let Some(err) = &service.tail_error {
        // No leading "  " — the block padding handles it, including
        // for wrapped continuation lines.
        vec![
            Line::from(Span::styled(
                format!("log tail unavailable: {err}"),
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "set runtime.backend = \"compose\" in keel.toml",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "and ensure docker compose is on PATH.",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        lines
    };

    let paragraph = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn service_status_spans(service: &ServicePane) -> Vec<Span<'static>> {
    if service.tail_error.is_some() {
        return vec![Span::styled(
            "  ✗ unavailable ".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )];
    }
    match service.status {
        Some(ServiceStatus::Running) => vec![Span::styled(
            "  ● running ".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )],
        Some(ServiceStatus::Stopped) => vec![Span::styled(
            "  ○ stopped ".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )],
        Some(ServiceStatus::Missing) => vec![Span::styled(
            "  ✗ missing ".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )],
        None => vec![Span::styled(
            "  ? unknown ".to_string(),
            Style::default().fg(Color::DarkGray),
        )],
    }
}

pub(crate) fn render_watcher(watcher: &WatcherPane, accent: Color, frame: &mut Frame, area: Rect) {
    // Watcher meta (recipe / globs / debounce) lives in the info
    // panel above. The output pane is purely the buffer from the
    // most recent run, with a placeholder when nothing has run yet —
    // matching the recipe / script / lifecycle pane shape.
    let title = output_pane_title(OutputStatus::Watcher(watcher), accent);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));

    let max_lines = area.height.saturating_sub(2) as usize;
    let total = watcher.buffer.len();
    let start = total.saturating_sub(max_lines);
    let captured: Vec<Line<'static>> = watcher
        .buffer
        .iter()
        .skip(start)
        .map(render_captured_line)
        .collect();

    let body = if captured.is_empty() {
        let placeholder = match watcher.state {
            WatcherState::Idle if watcher.last_exit_code.is_none() => {
                "no run yet — edit a watched file"
            }
            WatcherState::Idle => "idle — buffer cleared on next run",
            WatcherState::Debouncing => "cooldown…",
            WatcherState::Running => "starting…",
        };
        vec![Line::from(Span::styled(
            placeholder.to_string(),
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        captured
    };

    let paragraph = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn watcher_status_spans(watcher: &WatcherPane, accent: Color) -> Vec<Span<'static>> {
    match watcher.state {
        WatcherState::Idle => match watcher.last_exit_code {
            None => vec![Span::styled(
                "  ○ idle ".to_string(),
                Style::default().fg(Color::DarkGray),
            )],
            Some(0) => vec![Span::styled(
                "  ✓ idle (last 0) ".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )],
            Some(c) => vec![Span::styled(
                format!("  ✗ idle (last {c}) "),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )],
        },
        WatcherState::Debouncing => {
            let remaining = watcher.pending_remaining_ms().unwrap_or(0);
            vec![Span::styled(
                format!("  ◐ cooldown {remaining} ms "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]
        }
        WatcherState::Running => {
            let elapsed = watcher
                .last_run_started_at
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            vec![Span::styled(
                format!("  ● running {elapsed:.1}s "),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )]
        }
    }
}

pub(crate) fn watcher_indicator_style(app: &App, name: &str) -> Style {
    let Some(w) = app.watchers().get(name) else {
        return Style::default().fg(Color::DarkGray);
    };
    match w.state {
        WatcherState::Idle => match w.last_exit_code {
            None => Style::default().fg(Color::DarkGray),
            Some(0) => Style::default().fg(Color::Green),
            Some(_) => Style::default().fg(Color::Red),
        },
        WatcherState::Debouncing => Style::default().fg(Color::Yellow),
        WatcherState::Running => Style::default().fg(accent_of(app)),
    }
}

pub(crate) fn service_indicator_style(app: &App, service: &str) -> Style {
    if let Some(pane) = app.services().get(service)
        && pane.tail_error.is_some()
    {
        return Style::default().fg(Color::Red);
    }
    match app.services().get(service).and_then(|p| p.status) {
        Some(ServiceStatus::Running) => Style::default().fg(Color::Green),
        Some(ServiceStatus::Stopped) => Style::default().fg(Color::Yellow),
        Some(ServiceStatus::Missing) => Style::default().fg(Color::Red),
        None => Style::default().fg(Color::DarkGray),
    }
}

pub(crate) fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    let h = height.min(area.height);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(h) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(area);
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h_layout[1]
}

pub(crate) fn window(selected: usize, visible: usize, total: usize) -> (usize, usize) {
    if total <= visible {
        return (0, total);
    }
    let half = visible / 2;
    let start = selected.saturating_sub(half).min(total - visible);
    let end = (start + visible).min(total);
    (start, end)
}

// ─────────────────────── status / hint bar ───────────────────────

/// Hint set for the active view. Phases 3 and 4 fill in their own
/// view-specific hints; for now the placeholder views just show
/// the global view-switch keys.
fn view_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.view() {
        crate::tui::app::View::ControlCenter => control_center_hints(app),
        crate::tui::app::View::Terminals => {
            let mut h: Vec<(&'static str, &'static str)> = vec![
                ("↑↓", "nav"),
                ("enter", "attach"),
                ("n", "new"),
                ("d", "delete"),
                ("C", "control"),
                ("G", "diff"),
                ("W", "worktree"),
            ];
            if app.editor().mode == crate::tui::editor::LaunchMode::Gui {
                h.push(("E", "open ide"));
            }
            h.push(("q", "quit"));
            h
        }
        crate::tui::app::View::Diff => diff_hints(app),
    }
}

fn diff_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let in_read = app.diff().body_mode() == BodyMode::Read;
    let mut hints: Vec<(&'static str, &'static str)> = match app.diff().focus() {
        DiffFocus::Files => {
            let mut h: Vec<(&'static str, &'static str)> = vec![("↑↓", "file"), ("tab", "body")];
            if !in_read {
                h.push(("]/[", "hunk"));
            }
            // `e` opens a single file — works for any editor (terminal
            // or GUI), so no mode gate here.
            h.push(("e", "edit"));
            h
        }
        DiffFocus::Body => {
            let mut h: Vec<(&'static str, &'static str)> = vec![("↑↓", "scroll"), ("tab", "files")];
            if !in_read {
                h.push(("]/[", "hunk"));
            }
            h.push(("gg/G", "top/bot"));
            h
        }
    };
    hints.push(match app.diff().body_mode() {
        BodyMode::Diff => ("v", "read"),
        BodyMode::Read => ("v", "diff"),
    });
    hints.push(("w", "wrap"));
    if app.diff().lazygit_available {
        hints.push(("L", "lazygit"));
    }
    hints.push(("r", "refresh"));
    hints.push(("C", "control"));
    hints.push(("T", "terminals"));
    hints.push(("W", "worktree"));
    if app.editor().mode == crate::tui::editor::LaunchMode::Gui {
        hints.push(("E", "open ide"));
    }
    hints.push(("q", "quit"));
    hints
}

fn control_center_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let mode_palette = app.mode() == Mode::Palette;

    let mut hints: Vec<(&str, &str)> = vec![("↑↓", "nav")];
    if !mode_palette {
        // Enter label adapts to the row kind so users see what
        // pressing it will do without consulting docs.
        let enter_label = match app.selected_item().map(|i| i.kind) {
            Some(ItemKind::Runtime) => "up all",
            Some(ItemKind::Service) => "attach",
            Some(ItemKind::Recipe | ItemKind::Script) => "run",
            _ => "select",
        };
        hints.push(("enter", enter_label));
        // Combined single/all keybinds — lowercase acts on the
        // selected service, uppercase on every service. Pairing
        // them in one hint keeps the legend short.
        hints.push(("u/U", "up"));
        hints.push(("r/R", "restart"));
        hints.push(("s/S", "stop"));
        hints.push(("D", "down all"));
        // View / worktree switches at the end so they never compete
        // with the action keys above for short-list real estate.
        hints.push(("T", "terminals"));
        hints.push(("G", "diff"));
        hints.push(("W", "worktree"));
        if app.editor().mode == crate::tui::editor::LaunchMode::Gui {
            hints.push(("E", "open ide"));
        }
    }
    hints.push(("/", "palette"));
    hints.push(("q", "quit"));
    hints
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(flash) = app.flash_message() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" ! ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(flash.to_string(), Style::default().fg(Color::Yellow)),
        ]));
        frame.render_widget(p, area);
        return;
    }

    let hints = view_hints(app);
    let accent = accent_of(app);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(hints.len() * 4 + 4);
    // Leading view tag — `[control]` / `[terminals]` / `[diff]`.
    // Anchors the user to which view their hints apply to.
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("[{}]", app.view().tag()),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw(" "));
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            (*label).to_string(),
            Style::default().fg(Color::White).dim(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ─────────────────────── helpers ───────────────────────

pub(crate) fn panel_block(title: &'static str, accent: Color) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

pub(crate) fn panel_block_titled(title: Line<'static>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::views::diff::state::{DiffFile, DiffStatus};
    use crate::tui::views::terminals::state::TmuxWindow;
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    fn cfg() -> Arc<crate::config::Config> {
        Arc::new(
            crate::config::parse_str(
                r#"
                [project]
                name = "tuitest"

                [command.up]
                desc = "Start"
                run = "docker compose up"

                [command.test]
                run = "composer test"
                in = "app"
                forward_args = true

                [[ui.pane]]
                type = "service"
                service = "app"

                [[ui.pane]]
                type = "watcher"
                glob = ["*.rs"]
                on_change = "test"
            "#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(cfg());
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn wrap_words_breaks_at_word_boundary() {
        let out = wrap_words("the quick brown fox jumps", 10);
        assert_eq!(out, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn wrap_words_oversize_word_keeps_alone() {
        // A single word larger than the width gets its own line; we
        // don't break inside a word.
        let out = wrap_words("foo supercalifragilistic bar", 8);
        assert_eq!(out, vec!["foo", "supercalifragilistic", "bar"]);
    }

    #[test]
    fn wrap_words_collapses_whitespace() {
        // Multiple internal spaces become single spaces (split_whitespace).
        let out = wrap_words("a   b\tc", 10);
        assert_eq!(out, vec!["a b c"]);
    }

    #[test]
    fn wrap_words_empty_input() {
        assert_eq!(wrap_words("", 10), Vec::<String>::new());
    }

    /// Smoke test: open the switcher → switcher_confirm on the
    /// "+ new worktree" sentinel → open_create_form. Render must
    /// not panic, and the rendered buffer must contain the form's
    /// title chrome plus at least one branch row.
    #[test]
    fn renders_create_worktree_form_with_branches() {
        use crate::tui::dialogs::switcher::WorktreeRow;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg());
        let entries = vec![WorktreeRow {
            path: std::path::PathBuf::from("/repo"),
            branch: Some("main".into()),
            slug: "main".into(),
            is_current: true,
        }];
        app.open_worktree_switcher(entries);
        // Move to "+ new worktree" sentinel and confirm.
        if let Some(s) = app.switcher_mut() {
            s.select_next();
        };
        app.switcher_confirm();
        // Provide a couple of fake branches.
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
            ],
            None,
        );
        terminal.draw(|f| render(&app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("new worktree"), "title missing:\n{text}");
        assert!(text.contains("main"), "branch row missing:\n{text}");
        assert!(text.contains("feat-x"), "second branch missing:\n{text}");
    }

    #[test]
    fn renders_empty_config() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(Arc::new(crate::config::Config::default()));
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn top_bar_shows_branch_when_set() {
        let backend = TestBackend::new(120, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(cfg()).with_branch(Some("feature-x".into()));
        terminal.draw(|f| render(&app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, 0)].symbol());
        }
        assert!(
            text.contains("feature-x"),
            "expected branch in top bar, got:\n{text}"
        );
    }

    #[test]
    fn top_bar_shows_dirty_count_when_files_present() {
        let backend = TestBackend::new(120, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg()).with_branch(Some("main".into()));
        app.diff_mut().set_files(vec![
            DiffFile {
                path: "src/a.rs".into(),
                status: DiffStatus::Modified,
                additions: 0,
                deletions: 0,
                binary: false,
                old_path: None,
            },
            DiffFile {
                path: "src/b.rs".into(),
                status: DiffStatus::Added,
                additions: 0,
                deletions: 0,
                binary: false,
                old_path: None,
            },
        ]);
        terminal.draw(|f| render(&app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, 0)].symbol());
        }
        assert!(
            text.contains("●2"),
            "expected dirty marker '●2' in top bar, got:\n{text}"
        );
    }

    #[test]
    fn top_bar_omits_branch_when_unset() {
        // No branch (e.g. not in a git repo) → header skips the slot
        // entirely rather than showing an empty separator.
        let backend = TestBackend::new(120, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(cfg());
        terminal.draw(|f| render(&app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, 0)].symbol());
        }
        // Project name renders, but no second separator after it.
        assert!(text.contains("tuitest"), "expected project name");
        assert!(
            !text.contains("●"),
            "should not show dirty marker without a branch context"
        );
    }

    #[test]
    fn renders_with_palette_open() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg());
        app.open_palette();
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    /// Reproduces the user's report: enter terminals view, create a
    /// new shell, detach, refresh shows 1 window. The sidebar must
    /// render that window — if the row is missing the test asserts
    /// here. Buffer dump on failure tells us where it actually
    /// landed.
    #[test]
    fn terminals_view_renders_a_window_row() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg());
        app.terminals_mut().set_tmux_available(true);
        app.switch_view(crate::tui::app::View::Terminals);
        app.terminals_set_windows(vec![TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        terminal.draw(|f| render(&app, f)).unwrap();

        // Dump the rendered buffer into a string so we can search
        // it for the window's name. The test fixture has no
        // services, so the row should land in the only sidebar
        // group.
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("zsh"),
            "expected 'zsh' in rendered sidebar, got:\n{text}"
        );
        assert!(
            text.contains("new shell"),
            "expected '+ new shell' sentinel, got:\n{text}"
        );
        // Baseline: no bell → diamond glyph, no filled dot in the
        // sidebar (the dot only appears for service status, which
        // lives elsewhere in the layout).
        assert!(
            text.contains('◇'),
            "expected diamond glyph for bell-less window, got:\n{text}"
        );
    }

    /// When a window has `has_bell` set, the sidebar swaps the
    /// diamond glyph for a filled dot — the visual cue that the
    /// terminal is waiting on the user (e.g. a coding agent rang
    /// the bell).
    #[test]
    fn terminals_view_marks_window_with_pending_bell() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg());
        app.terminals_mut().set_tmux_available(true);
        app.switch_view(crate::tui::app::View::Terminals);
        app.terminals_set_windows(vec![TmuxWindow {
            index: 0,
            name: "claude".into(),
            cwd: None,
            has_bell: true,
            workspace: None,
        }]);
        terminal.draw(|f| render(&app, f)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains('●'),
            "expected filled dot for bell-set window, got:\n{text}"
        );
        assert!(
            !text.contains('◇'),
            "diamond should be replaced by dot when bell is set, got:\n{text}"
        );
    }

    /// Same as the previous test but with services in the sidebar
    /// (matching the user's tmp/test fixture which auto-discovers
    /// `app` and `worker` from docker-compose). The terminals
    /// group has to share vertical real estate with services here
    /// — if the constraint math is wrong, the window row gets
    /// pushed out of the visible area.
    #[test]
    fn terminals_view_renders_window_alongside_services() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let cfg = Arc::new(
            crate::config::parse_str(
                r#"
                [project]
                name = "tuitest"

                [runtime]
                backend = "none"

                [[ui.pane]]
                type    = "service"
                service = "app"

                [[ui.pane]]
                type    = "service"
                service = "worker"
            "#,
            )
            .unwrap(),
        );
        let mut app = App::new(cfg);
        app.terminals_mut().set_tmux_available(true);
        app.switch_view(crate::tui::app::View::Terminals);
        app.terminals_set_windows(vec![TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
            has_bell: false,
            workspace: None,
        }]);
        terminal.draw(|f| render(&app, f)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("zsh"),
            "expected 'zsh' alongside services, got:\n{text}"
        );
        // Always print so we can eyeball the actual layout — passes
        // either way; failure already prints text.
        println!("---rendered buffer---\n{text}---end---");
    }
}
