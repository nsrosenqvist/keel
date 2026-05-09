//! Rendering — pure function from [`App`] state to a ratatui [`Frame`].
//!
//! Layout is three vertical bands plus an optional palette overlay:
//!
//! ```text
//! ┌─ top status bar ───────────────────────────────────────────────┐
//! │ scaffl · <project> · worktree:<slug> · offset:<n>              │
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

use crate::app::{App, Item, ItemKind, Mode};
use crate::palette::Palette;
use crate::runner::{CapturedLine, RunState};
use crate::services::ServicePane;
use crate::watchers::{WatcherPane, WatcherState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Padding,
        Paragraph, Wrap,
    },
};
use scaffl_container::ServiceStatus;
use scaffl_runtime::OutputStream;

const SIDEBAR_RATIO: u16 = 28;
const TOP_BAR_HEIGHT: u16 = 1;
const STATUS_BAR_HEIGHT: u16 = 1;
/// Fixed height of the details panel under the sidebar list. Big
/// enough for the kind tag, desc, in, and run line; smaller details
/// (env, needs) wrap or get cut by the panel border.
const DETAILS_HEIGHT: u16 = 12;

/// Accent colour used for active highlights and key hints.
const ACCENT: Color = Color::Cyan;

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

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(outer[1]);

    // Left column: list on top, static details panel below.
    // Details panel is fixed-height — the list absorbs the rest.
    // Hidden entirely when there's no item to describe.
    let left = if app.selected_item().is_some() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(DETAILS_HEIGHT)])
            .split(body[0])
    } else {
        std::rc::Rc::from([body[0]])
    };
    render_sidebar(app, frame, left[0]);
    if left.len() > 1 {
        render_details(app, frame, left[1]);
    }

    render_right_pane(app, frame, body[1]);
    render_status(app, frame, outer[2]);

    if app.mode() == Mode::Palette
        && let Some(palette) = app.palette()
    {
        render_palette(app, palette, frame);
    }
    if app.mode() == Mode::Confirm
        && let Some(dialog) = app.confirm_dialog()
    {
        render_confirm_modal(dialog, frame);
    }
}

// ───────────────────────── top bar ─────────────────────────

fn render_top_bar(app: &App, frame: &mut Frame, area: Rect) {
    let project = app
        .config()
        .project
        .name
        .clone()
        .unwrap_or_else(|| "scaffl".into());

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(
            "  scaffl ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(project, Style::default().add_modifier(Modifier::BOLD)),
    ];

    // Worktree info — only meaningful when isolated.
    let services_count = app.services().len();
    let watchers_count = app.watchers().len();
    if services_count > 0 || watchers_count > 0 {
        spans.push(Span::styled(
            format!("  │  {services_count} services, {watchers_count} watchers"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let p = Paragraph::new(Line::from(spans));
    frame.render_widget(p, area);
}

// ─────────────────────── sidebar ───────────────────────

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let groups = build_groups(app);
    if groups.is_empty() {
        let block = panel_block(" commands ");
        let body = Paragraph::new(Line::from(Span::styled(
            "  (no items)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(body, area);
        return;
    }

    // Each section gets `items + 2` rows (top + bottom border, header in
    // the title). The last group absorbs the slack so we don't overflow
    // the sidebar.
    let last = groups.len() - 1;
    let constraints: Vec<Constraint> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            if i == last {
                Constraint::Min((g.len() as u16).saturating_add(2))
            } else {
                Constraint::Length((g.len() as u16).saturating_add(2))
            }
        })
        .collect();
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let global_idx = app.selected_index();
    let mut cursor = 0;
    for (group, group_area) in groups.iter().zip(areas.iter()) {
        let local_selected = if global_idx >= cursor && global_idx < cursor + group.len() {
            Some(global_idx - cursor)
        } else {
            None
        };
        render_group(app, group, *group_area, local_selected, frame);
        cursor += group.len();
    }
}

struct SidebarGroup<'a> {
    label: &'static str,
    items: Vec<&'a Item>,
}

impl<'a> SidebarGroup<'a> {
    fn len(&self) -> usize {
        self.items.len()
    }
}

fn build_groups(app: &App) -> Vec<SidebarGroup<'_>> {
    // Recipes and scripts share a single "commands" group in the
    // sidebar — to most users they're the same thing (a runnable
    // command), and the distinction is still visible where it matters
    // (kind tag in the palette and detail pane). Sidebar order
    // preserves the iteration order of `app.items()` within the
    // commands group, which is recipes (alphabetical) then scripts
    // (alphabetical) — natural enough for browsing.
    let mut services = Vec::new();
    let mut watchers = Vec::new();
    let mut commands = Vec::new();
    for item in app.items() {
        match item.kind {
            ItemKind::Service => services.push(item),
            ItemKind::Watcher => watchers.push(item),
            ItemKind::Recipe | ItemKind::Script => commands.push(item),
        }
    }
    let mut out = Vec::new();
    if !services.is_empty() {
        out.push(SidebarGroup {
            label: "services",
            items: services,
        });
    }
    if !watchers.is_empty() {
        out.push(SidebarGroup {
            label: "watchers",
            items: watchers,
        });
    }
    if !commands.is_empty() {
        out.push(SidebarGroup {
            label: "commands",
            items: commands,
        });
    }
    out
}

fn render_group(
    app: &App,
    group: &SidebarGroup<'_>,
    area: Rect,
    selected: Option<usize>,
    frame: &mut Frame,
) {
    let title_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            group.label,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", group.items.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = panel_block_titled(title_line);

    let list_items: Vec<ListItem> = group
        .items
        .iter()
        .map(|item| {
            let glyph = glyph_for(item.kind);
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
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(list_items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ")
        // Reserve the gutter for the indicator on every row so
        // unselected items align with the selected one. Without this,
        // changing selection horizontally shifts the rest of the list.
        .highlight_spacing(HighlightSpacing::Always);

    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
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

fn glyph_for(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Service => "●",
        ItemKind::Watcher => "◇",
        // Same glyph for recipes and scripts — they share the
        // "commands" sidebar group. Kind is still distinguishable in
        // the detail pane and palette tags.
        ItemKind::Recipe | ItemKind::Script => "▸",
    }
}

fn item_indicator_style(app: &App, item: &Item) -> Style {
    match item.kind {
        ItemKind::Service => service_indicator_style(app, &item.name),
        ItemKind::Watcher => watcher_indicator_style(app, &item.name),
        _ => Style::default().fg(Color::DarkGray),
    }
}

// ─────────────────────── right pane ───────────────────────

fn render_right_pane(app: &App, frame: &mut Frame, area: Rect) {
    // Right pane is the per-row buffer view. Lifecycle actions
    // (compose up / down / etc.) overlay as a bottom split when in
    // flight — they're project-wide and don't have a row of their
    // own. When idle (no lifecycle run), the pane is full-height.
    if app.lifecycle_run().is_some() {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        render_focused_buffer(app, frame, split[0]);
        render_lifecycle_output(app, frame, split[1]);
    } else {
        render_focused_buffer(app, frame, area);
    }
}

/// Paint the buffer for whatever's selected. Each kind has its own
/// source: services tail through their pane, watchers carry a buffer
/// from their last fire, recipes / scripts have their own RunState
/// in the runs map. The "tmux session per row" model.
fn render_focused_buffer(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(service) = app.selected_service() {
        render_service_logs(service, frame, area);
    } else if let Some(watcher) = app.selected_watcher() {
        render_watcher(watcher, frame, area);
    } else if let Some(run) = app.selected_run() {
        render_run_buffer(run, frame, area);
    } else if let Some(item) = app.selected_item() {
        render_idle_buffer(item, frame, area);
    } else {
        let block = panel_block(" output ");
        frame.render_widget(block, area);
    }
}

/// Placeholder for a recipe / script that's never been launched in
/// this session. Tells the user how to start it without leaving the
/// pane empty.
fn render_idle_buffer(item: &Item, frame: &mut Frame, area: Rect) {
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  not yet run".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));
    let body = Paragraph::new(Line::from(Span::styled(
        "press enter to run",
        Style::default().fg(Color::DarkGray),
    )))
    .block(block);
    frame.render_widget(body, area);
}

fn render_run_buffer(run: &RunState, frame: &mut Frame, area: Rect) {
    let title = run_pane_title(run);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));
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

fn render_lifecycle_output(app: &App, frame: &mut Frame, area: Rect) {
    let Some(run) = app.lifecycle_run() else {
        return;
    };
    let title = run_pane_title(run);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));
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

// ─────────────────────── details panel ───────────────────────

/// The static "what is this" box that lives under the sidebar list.
/// Mirrors the data the old right-pane "details" view rendered, but
/// limited to recipe / script / service / watcher metadata — no
/// dynamic state (timing / exit code), since the output pane title
/// already carries that.
fn render_details(app: &App, frame: &mut Frame, area: Rect) {
    let block = panel_block(" details ");
    // Description gets the panel's full inner width minus the kv
    // indent (2 cols) and the right border breathing room (1 col).
    // We pre-wrap it ourselves because ratatui's `Wrap` would
    // also wrap the kv lines, where it loses the hanging indent.
    let inner_width = area.width.saturating_sub(2 + 2 + 1) as usize;
    let paragraph = Paragraph::new(build_detail_lines(app, inner_width)).block(block);
    frame.render_widget(paragraph, area);
}

/// New layout (per user feedback):
///   <name>  <kind>          ← header
///                           ← blank
///   <description...>        ← wrapped paragraph, full width, no `desc` label
///   <continuation...>
///                           ← blank
///   in              host    ← kv lines
///   forward_args    true
///
/// `run` is omitted entirely — long run-strings dominated the panel,
/// and the description should explain intent at a higher level than
/// the literal command anyway.
fn build_detail_lines(app: &App, wrap_width: usize) -> Vec<Line<'static>> {
    let Some(item) = app.selected_item() else {
        return vec![Line::from(Span::styled(
            "  No commands defined.",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            kind_label(item.kind),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));

    let (desc, kv_lines) = match item.kind {
        ItemKind::Recipe => app
            .selected_recipe()
            .map(|r| (r.desc.clone(), recipe_kv_lines(r)))
            .unwrap_or_default(),
        ItemKind::Script => app
            .selected_script()
            .map(|s| (s.desc.clone(), script_kv_lines(s)))
            .unwrap_or_default(),
        // Services / watchers don't have config-level descriptions.
        _ => (None, Vec::new()),
    };

    if let Some(text) = desc {
        lines.push(Line::from(""));
        for chunk in wrap_words(&text, wrap_width) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(chunk, Style::default().fg(Color::Gray)),
            ]));
        }
    }

    if !kv_lines.is_empty() {
        lines.push(Line::from(""));
        lines.extend(kv_lines);
    }

    lines
}

fn recipe_kv_lines(recipe: &scaffl_config::Recipe) -> Vec<Line<'static>> {
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

fn script_kv_lines(script: &scaffl_config::ScriptCommand) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(kv("in", script.service.as_deref().unwrap_or("host")));
    let path_display = script
        .path
        .file_name()
        .map(|f| format!(".scaffl/commands/{}", f.to_string_lossy()))
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

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Service => "service",
        ItemKind::Watcher => "watcher",
        ItemKind::Recipe => "recipe",
        ItemKind::Script => "script",
    }
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<14}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

// ─────────────────────── service / watcher panes ───────────────────────

fn run_pane_title(run: &RunState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            run.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    let duration = format_duration(run.duration());
    if let Some(code) = run.exit_code {
        if code == 0 {
            spans.push(Span::styled(
                format!("✓ exit 0 · {duration} "),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("✗ exit {code} · {duration} "),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }
    } else if let Some(err) = &run.error {
        spans.push(Span::styled(
            format!("! {err} · {duration} "),
            Style::default().fg(Color::Red),
        ));
    } else {
        spans.push(Span::styled(
            format!("● running · {duration} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
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
    let style = match line.stream {
        OutputStream::Stdout => Style::default(),
        OutputStream::Stderr => Style::default().fg(Color::Red),
    };
    Line::from(Span::styled(line.text.clone(), style))
}

fn render_service_logs(service: &ServicePane, frame: &mut Frame, area: Rect) {
    let title = service_pane_title(service);

    // The error path is the only place this pane prints prose that's
    // long enough to wrap. Apply local block padding so the wrapped
    // continuation lines keep the indent — the leading "  " hack used
    // elsewhere only pads the first visual line. Other paths fall back
    // to the bare panel block so streaming compose logs render flush
    // (artificial indent would misalign the upstream tool's output).
    let block = if service.tail_error.is_some() {
        panel_block_titled(title).padding(Padding::new(2, 1, 1, 0))
    } else {
        panel_block_titled(title)
    };

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
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  waiting for output…",
                Style::default().fg(Color::DarkGray),
            )),
        ]
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
                "set containers.backend = \"compose\" in scaffl.toml",
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

fn service_pane_title(service: &ServicePane) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            service.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if service.tail_error.is_some() {
        spans.push(Span::styled(
            "✗ unavailable ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else {
        match service.status {
            Some(ServiceStatus::Running) => spans.push(Span::styled(
                "● running ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Some(ServiceStatus::Stopped) => spans.push(Span::styled(
                "○ stopped ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Some(ServiceStatus::Missing) => spans.push(Span::styled(
                "✗ missing ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            None => spans.push(Span::styled(
                "? unknown ",
                Style::default().fg(Color::DarkGray),
            )),
        }
    }
    Line::from(spans)
}

fn render_watcher(watcher: &WatcherPane, frame: &mut Frame, area: Rect) {
    let title = watcher_pane_title(watcher);
    let block = panel_block_titled(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(1)])
        .split(inner);

    let mut header_lines: Vec<Line<'static>> = Vec::new();
    header_lines.push(Line::from(""));
    header_lines.push(kv("recipe", &watcher.recipe));
    header_lines.push(kv("globs", &watcher.globs.join(", ")));
    header_lines.push(kv(
        "debounce",
        &format!("{} ms", watcher.debounce.as_millis()),
    ));
    if let Some(c) = watcher.last_exit_code {
        header_lines.push(kv("last_exit", &c.to_string()));
    }
    frame.render_widget(Paragraph::new(header_lines), layout[0]);

    let max_lines = layout[1].height as usize;
    let total = watcher.buffer.len();
    let start = total.saturating_sub(max_lines);
    let lines: Vec<Line<'static>> = watcher
        .buffer
        .iter()
        .skip(start)
        .map(render_captured_line)
        .collect();

    let body = if lines.is_empty() {
        let placeholder = match watcher.state {
            WatcherState::Idle if watcher.last_exit_code.is_none() => {
                "(no run yet — edit a watched file)".to_string()
            }
            WatcherState::Idle => "(idle — buffer cleared on next run)".into(),
            WatcherState::Debouncing => "(cooldown…)".into(),
            WatcherState::Running => "(starting…)".into(),
        };
        vec![Line::from(Span::styled(
            placeholder,
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
    };
    // Borderless inner block solely to add the same gutter the run
    // output uses. Keeps captured recipe stdout off the border
    // without affecting the kv-style header above. The 1-row top
    // padding gives the body breathing room from the header.
    let body_block = Block::default().padding(Padding::new(2, 1, 1, 0));
    let paragraph = Paragraph::new(body)
        .block(body_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, layout[1]);
}

fn watcher_pane_title(watcher: &WatcherPane) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            watcher.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    match watcher.state {
        WatcherState::Idle => match watcher.last_exit_code {
            None => spans.push(Span::styled(
                "○ idle ",
                Style::default().fg(Color::DarkGray),
            )),
            Some(0) => spans.push(Span::styled(
                "✓ idle (last 0) ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Some(c) => spans.push(Span::styled(
                format!("✗ idle (last {c}) "),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
        },
        WatcherState::Debouncing => {
            let remaining = watcher.pending_remaining_ms().unwrap_or(0);
            spans.push(Span::styled(
                format!("◐ cooldown {remaining} ms "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        WatcherState::Running => {
            let elapsed = watcher
                .last_run_started_at
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            spans.push(Span::styled(
                format!("● running {elapsed:.1}s "),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
        }
    }
    Line::from(spans)
}

fn watcher_indicator_style(app: &App, name: &str) -> Style {
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
        WatcherState::Running => Style::default().fg(ACCENT),
    }
}

fn service_indicator_style(app: &App, service: &str) -> Style {
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

// ─────────────────────── palette ───────────────────────

fn render_palette(app: &App, palette: &Palette, frame: &mut Frame) {
    let outer = frame.area();
    let area = centered_rect(outer, 60, 16);
    frame.render_widget(Clear, area);

    let block = panel_block_titled(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "run …",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Esc cancels ", Style::default().fg(Color::DarkGray)),
    ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    let input_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "❯ ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            palette.input().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ACCENT)),
    ]);
    frame.render_widget(Paragraph::new(input_line), layout[0]);

    let visible = layout[1].height as usize;
    let total = palette.matches().len();
    let selected = palette.selected();
    let (start, end) = window(selected, visible, total);

    let lines: Vec<Line<'static>> = palette
        .matches()
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .map(|(idx, m)| {
            let item = &app.items()[m.item_index];
            let kind = kind_label(item.kind);
            let row_style = if idx == selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    if idx == selected { "▶ " } else { "  " }.to_string(),
                    row_style,
                ),
                Span::styled(format!("{:<24} ", item.name), row_style),
                Span::styled(
                    format!("[{kind}]"),
                    if idx == selected {
                        row_style
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
            ])
        })
        .collect();

    let body = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "  (no matches)".to_string(),
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
    };
    frame.render_widget(Paragraph::new(body), layout[1]);
}

fn render_confirm_modal(dialog: &crate::app::ConfirmDialog, frame: &mut Frame) {
    // Center a fixed-size box. Width is generous enough to fit the
    // longest plausible body line; height is just the four content
    // rows + borders. Anything narrower than ~60 cols falls back to
    // the area width to avoid clipping.
    let area = centered_rect(frame.area(), 50, 7);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                dialog.title.clone(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let yes_style = if dialog.yes_focused {
        Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let no_style = if !dialog.yes_focused {
        Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let body = vec![
        Line::from(Span::raw(dialog.body.clone())),
        Line::from(""),
        Line::from(vec![
            Span::styled(" Yes ", yes_style),
            Span::raw("    "),
            Span::styled(" No ", no_style),
        ]),
    ];

    // Clear behind the modal so the underlying content doesn't bleed
    // through the rounded corners.
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(body).block(block), area);
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
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

fn window(selected: usize, visible: usize, total: usize) -> (usize, usize) {
    if total <= visible {
        return (0, total);
    }
    let half = visible / 2;
    let start = selected.saturating_sub(half).min(total - visible);
    let end = (start + visible).min(total);
    (start, end)
}

// ─────────────────────── status / hint bar ───────────────────────

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(flash) = &app.flash {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" ! ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(flash.clone(), Style::default().fg(Color::Yellow)),
        ]));
        frame.render_widget(p, area);
        return;
    }

    // "Something running" = the selected row's run, or the lifecycle
    // slot. Used to flip the `s` hint between "stop" and "stop run."
    let running = app
        .selected_run()
        .is_some_and(|r| !r.is_done())
        || app.lifecycle_run().is_some_and(|r| !r.is_done());
    let mode_palette = app.mode() == Mode::Palette;
    let on_service = app.selected_service().is_some();

    let mut hints: Vec<(&str, &str)> = vec![("↑↓", "nav")];
    if !mode_palette {
        if on_service {
            hints.push(("enter", "up"));
            hints.push(("r", "restart"));
            if running {
                hints.push(("s", "stop run"));
            } else {
                hints.push(("s", "stop"));
            }
        } else {
            hints.push(("enter", "run"));
            if running {
                hints.push(("s", "stop run"));
            }
        }
        // Project-wide service ops. Single rule: shift = act on all.
        hints.push(("U", "up all"));
        hints.push(("R", "restart all"));
        hints.push(("S", "stop all"));
        hints.push(("D", "down all"));
    }
    hints.push(("/  :", "palette"));
    hints.push(("q", "quit"));

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(hints.len() * 4 + 1);
    spans.push(Span::raw(" "));
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
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

fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

fn panel_block_titled(title: Line<'static>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    fn cfg() -> Arc<scaffl_config::Config> {
        Arc::new(
            scaffl_config::parse_str(
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

    #[test]
    fn renders_empty_config() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(Arc::new(scaffl_config::Config::default()));
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn renders_with_palette_open() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(cfg());
        app.open_palette();
        terminal.draw(|f| render(&app, f)).unwrap();
    }
}
