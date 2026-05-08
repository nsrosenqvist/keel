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
use scaffl_config::Run;
use scaffl_container::ServiceStatus;
use scaffl_runtime::OutputStream;

const SIDEBAR_RATIO: u16 = 28;
const TOP_BAR_HEIGHT: u16 = 1;
const STATUS_BAR_HEIGHT: u16 = 1;

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

    render_sidebar(app, frame, body[0]);
    render_right_pane(app, frame, body[1]);
    render_status(app, frame, outer[2]);

    if app.mode() == Mode::Palette
        && let Some(palette) = app.palette()
    {
        render_palette(app, palette, frame);
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
    let mut services = Vec::new();
    let mut watchers = Vec::new();
    let mut recipes = Vec::new();
    let mut scripts = Vec::new();
    for item in app.items() {
        match item.kind {
            ItemKind::Service => services.push(item),
            ItemKind::Watcher => watchers.push(item),
            ItemKind::Recipe => recipes.push(item),
            ItemKind::Script => scripts.push(item),
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
    if !recipes.is_empty() {
        out.push(SidebarGroup {
            label: "recipes",
            items: recipes,
        });
    }
    if !scripts.is_empty() {
        out.push(SidebarGroup {
            label: "scripts",
            items: scripts,
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
            ListItem::new(Line::from(vec![
                Span::styled(format!("{glyph} "), glyph_style),
                Span::raw(item.name.clone()),
            ]))
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

fn glyph_for(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Service => "●",
        ItemKind::Watcher => "◇",
        ItemKind::Recipe => "▸",
        ItemKind::Script => "▪",
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
    let has_run = app.current_run().is_some();
    if has_run {
        // Always split when a run is in progress / completed, no matter
        // what's selected, so action output (compose up / down /
        // recipe runs) is visible alongside the focused item's pane.
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_focused_item(app, frame, split[0]);
        render_output(app, frame, split[1]);
    } else {
        render_focused_item(app, frame, area);
    }
}

fn render_focused_item(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(service) = app.selected_service() {
        render_service_logs(service, frame, area);
    } else if let Some(watcher) = app.selected_watcher() {
        render_watcher(watcher, frame, area);
    } else {
        render_detail(app, frame, area);
    }
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let block = panel_block(" details ");
    let lines = build_detail_lines(app);
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn build_detail_lines(app: &App) -> Vec<Line<'static>> {
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
    lines.push(Line::from(""));

    if let Some(recipe) = app.selected_recipe() {
        if let Some(desc) = &recipe.desc {
            lines.push(kv("desc", desc));
        }
        lines.push(kv("in", recipe.service.as_deref().unwrap_or("host")));
        if recipe.tty {
            lines.push(kv("tty", "true"));
        }
        if recipe.forward_args {
            lines.push(kv("forward_args", "true"));
        }
        if recipe.parallel {
            lines.push(kv("parallel", "true"));
        }
        if !recipe.needs.is_empty() {
            lines.push(kv("needs", &recipe.needs.join(", ")));
        }
        for (k, v) in &recipe.env {
            lines.push(kv(&format!("env.{k}"), v));
        }
        lines.push(Line::from(""));
        lines.extend(render_run(&recipe.run));
    } else if app.selected_watcher().is_some() {
        // Watcher detail is rendered by render_watcher when this item
        // is the selection — render_right_pane picks the watcher path.
    } else if let Some(script) = app.selected_script() {
        if let Some(desc) = &script.desc {
            lines.push(kv("desc", desc));
        }
        lines.push(kv("in", script.service.as_deref().unwrap_or("host")));
        lines.push(kv("path", &script.path.display().to_string()));
        if script.tty {
            lines.push(kv("tty", "true"));
        }
        if script.forward_args {
            lines.push(kv("forward_args", "true"));
        }
        if !script.needs.is_empty() {
            lines.push(kv("needs", &script.needs.join(", ")));
        }
        for (k, v) in &script.env {
            lines.push(kv(&format!("env.{k}"), v));
        }
    }

    lines
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Service => "service",
        ItemKind::Watcher => "watcher",
        ItemKind::Recipe => "recipe",
        ItemKind::Script => "script",
    }
}

fn render_run(run: &Run) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "run",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ])];
    match run {
        Run::Single(s) => out.push(Line::from(Span::raw(format!("    {s}")))),
        Run::Steps(steps) => {
            for s in steps {
                out.push(Line::from(Span::raw(format!("    • {s}"))));
            }
        }
    }
    out
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<14}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

// ─────────────────────── output / service / watcher ───────────────────────

fn render_output(app: &App, frame: &mut Frame, area: Rect) {
    let Some(run) = app.current_run() else {
        return;
    };

    let title = run_pane_title(run);
    let block = panel_block_titled(title);

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

fn run_pane_title(run: &RunState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            run.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if let Some(code) = run.exit_code {
        if code == 0 {
            spans.push(Span::styled(
                "✓ exit 0 ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("✗ exit {code} "),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }
    } else if let Some(err) = &run.error {
        spans.push(Span::styled(
            format!("! {err} "),
            Style::default().fg(Color::Red),
        ));
    } else {
        let secs = run.started_at.elapsed().as_secs_f32();
        spans.push(Span::styled(
            format!("● running {secs:.1}s "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
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
    let block = panel_block_titled(title);

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
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  log tail unavailable: {err}"),
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  set runtime.backend = \"compose\" in scaffl.toml",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  and ensure docker compose is on PATH.",
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
                "  (no run yet — edit a watched file)".to_string()
            }
            WatcherState::Idle => "  (idle — buffer cleared on next run)".into(),
            WatcherState::Debouncing => "  (cooldown…)".into(),
            WatcherState::Running => "  (starting…)".into(),
        };
        vec![Line::from(Span::styled(
            placeholder,
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
    };
    let paragraph = Paragraph::new(body).wrap(Wrap { trim: false });
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

    let running = app.current_run().is_some_and(|r| !r.is_done());
    let mode_palette = app.mode() == Mode::Palette;
    let on_service = app.selected_service().is_some();

    let mut hints: Vec<(&str, &str)> = vec![("↑↓", "nav")];
    if !mode_palette {
        if on_service {
            hints.push(("enter", "up"));
            hints.push(("r", "restart"));
        } else {
            hints.push(("enter", "run"));
        }
    }
    hints.push(("u", "up"));
    hints.push(("d", "down"));
    if running {
        hints.push(("s", "stop run"));
    } else if on_service {
        hints.push(("s", "stop svc"));
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

/// Standard padding inside any bordered panel: one column of breathing
/// room on each horizontal edge, no vertical padding (panel content
/// already manages its own line spacing).
const PANEL_PADDING: Padding = Padding::new(1, 1, 0, 0);

fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .padding(PANEL_PADDING)
}

fn panel_block_titled(title: Line<'static>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .padding(PANEL_PADDING)
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
