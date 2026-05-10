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

    // Body delegated by view. Each view owns its sidebar + right
    // pane; only the chrome (top bar, status bar) is shared.
    match app.view() {
        crate::app::View::ControlCenter => render_control_center(app, frame, outer[1]),
        crate::app::View::Terminals => render_terminals_placeholder(app, frame, outer[1]),
        crate::app::View::Diff => render_diff_placeholder(app, frame, outer[1]),
    }

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
    if app.mode() == Mode::ArgsPrompt
        && let Some(prompt) = app.args_prompt()
    {
        render_args_prompt(prompt, frame);
    }
    if app.mode() == Mode::WorktreeSwitcher
        && let Some(switcher) = app.switcher()
    {
        render_worktree_switcher(switcher, frame);
    }
}

/// Control-center body: sidebar (list + details panel) + right pane.
/// Today's home view; everything below the top bar today.
fn render_control_center(app: &App, frame: &mut Frame, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);

    // Left column: list on top, static details panel below.
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
}

/// Real Terminals body: tmux-backed sidebar + info panel.
fn render_terminals_placeholder(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(false) = app.terminals().tmux_available {
        render_tmux_missing(frame, area);
        return;
    }
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);
    render_terminals_sidebar(app, frame, body[0]);
    render_terminals_info(app, frame, body[1]);
}

fn render_tmux_missing(frame: &mut Frame, area: Rect) {
    let block = panel_block(" terminals ");
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  tmux is required for the terminals view",
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  install it (`brew install tmux` / `apt install tmux`),",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  then press T again, or restart scaffl.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(block);
    frame.render_widget(body, area);
}

fn render_terminals_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let rows = app.terminals_rows();
    let selected = app.terminals().selected.min(rows.len().saturating_sub(1));

    // Split the sidebar into two stacked groups: services on top,
    // terminals + sentinel below. Group sizing mirrors the control
    // center's per-group constraints.
    let services_count = rows
        .iter()
        .filter(|r| matches!(r, crate::app::TerminalsRow::Service(_)))
        .count();
    let terminals_total = rows.len() - services_count;
    let constraints: Vec<Constraint> = if services_count > 0 {
        vec![
            Constraint::Length((services_count as u16).saturating_add(2)),
            Constraint::Min((terminals_total as u16).saturating_add(2)),
        ]
    } else {
        vec![Constraint::Min(1)]
    };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Highlight style shared by both groups — same shape as the
    // control-center sidebar so visited and selected rows read
    // consistently across views.
    let highlight = Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD);

    // Services group
    if services_count > 0 {
        let mut svc_items: Vec<ListItem> = Vec::new();
        let mut svc_selected: Option<usize> = None;
        for (local_idx, (global_idx, row)) in rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, crate::app::TerminalsRow::Service(_)))
            .enumerate()
        {
            let crate::app::TerminalsRow::Service(name) = row else {
                continue;
            };
            if global_idx == selected {
                svc_selected = Some(local_idx);
            }
            let glyph_style = service_indicator_style(app, name);
            svc_items.push(ListItem::new(Line::from(vec![
                Span::styled("● ", glyph_style),
                Span::raw(name.clone()),
            ])));
        }
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "services",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({services_count}) "),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let list = List::new(svc_items)
            .block(panel_block_titled(title))
            .highlight_style(highlight)
            .highlight_spacing(HighlightSpacing::Always);
        let mut state = ListState::default();
        state.select(svc_selected);
        frame.render_stateful_widget(list, areas[0], &mut state);
    }

    // Terminals + sentinel group
    let mut term_items: Vec<ListItem> = Vec::new();
    let mut term_selected: Option<usize> = None;
    for (global_idx, row) in rows.iter().enumerate() {
        match row {
            crate::app::TerminalsRow::Service(_) => continue,
            crate::app::TerminalsRow::Window(w) => {
                if global_idx == selected {
                    term_selected = Some(term_items.len());
                }
                term_items.push(ListItem::new(window_row_line(w)));
            }
            crate::app::TerminalsRow::NewSentinel => {
                if global_idx == selected {
                    term_selected = Some(term_items.len());
                }
                term_items.push(ListItem::new(Line::from(Span::styled(
                    "+ new shell",
                    Style::default().fg(ACCENT),
                ))));
            }
        }
    }
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "terminals",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", terminals_total - 1),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let area_for_terms = if services_count > 0 {
        areas[1]
    } else {
        areas[0]
    };
    let list = List::new(term_items)
        .block(panel_block_titled(title))
        .highlight_style(highlight)
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default();
    state.select(term_selected);
    frame.render_stateful_widget(list, area_for_terms, &mut state);
}

/// Render one window row in the terminals sidebar. Tmux's
/// automatic-rename keeps `name` in sync with the running command
/// (`zsh`, `vim`, …); when we have a `cwd` populated, we append
/// it for the kind of "tab title" feel a real terminal would
/// show. The cwd is collapsed against $HOME (`~`) to fit narrow
/// sidebars.
fn window_row_line(w: &crate::app::TmuxWindow) -> Line<'static> {
    let mut spans = vec![
        Span::styled("◇ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}: ", w.index),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(w.name.clone()),
    ];
    if let Some(cwd) = w.cwd.as_deref() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            collapse_home(cwd),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Collapse `$HOME` to `~` for friendlier display. Cheap; runs
/// once per render frame.
fn collapse_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if path == home {
            return "~".to_string();
        }
        let with_slash = format!("{home}/");
        if let Some(rest) = path.strip_prefix(&with_slash) {
            return format!("~/{rest}");
        }
    }
    path.to_string()
}

fn render_terminals_info(app: &App, frame: &mut Frame, area: Rect) {
    let rows = app.terminals_rows();
    let selected_row = rows.get(app.terminals().selected);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "tmux".to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  session: {}", app.terminals().session_name),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));

    // Try to surface a tmux pane preview when one exists — much
    // more useful than static "press enter to attach" text once
    // the user has attached at least once.
    let preview_index = match selected_row {
        Some(crate::app::TerminalsRow::Window(w)) => Some(w.index),
        Some(crate::app::TerminalsRow::Service(name)) => app
            .terminals()
            .windows
            .iter()
            .find(|w| w.name == format!("svc:{name}"))
            .map(|w| w.index),
        _ => None,
    };
    let preview = preview_index.and_then(|i| app.terminals_preview(i));

    let detach_hint = "ctrl+b d returns to scaffl";
    let mut lines: Vec<Line<'static>> = Vec::new();
    match selected_row {
        Some(crate::app::TerminalsRow::Service(name)) => match preview {
            Some(p) if !p.is_empty() => {
                lines.push(Line::from(Span::styled(
                    format!("svc:{name}  ·  last visible:"),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                let body_height = area.height.saturating_sub(6) as usize;
                lines.extend(preview_lines(p, body_height));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "enter re-attaches  ·  d closes the window",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            _ => {
                lines.push(Line::from(Span::styled(
                    format!("attach into service `{name}`"),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("→ docker compose exec -it {name} $SHELL"),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    detach_hint,
                    Style::default().fg(Color::DarkGray),
                )));
            }
        },
        Some(crate::app::TerminalsRow::Window(w)) => match preview {
            Some(p) if !p.is_empty() => {
                let header = match w.cwd.as_deref() {
                    Some(cwd) => format!("{}  ·  {}", w.name, collapse_home(cwd)),
                    None => w.name.clone(),
                };
                lines.push(Line::from(Span::styled(
                    header,
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                let body_height = area.height.saturating_sub(6) as usize;
                lines.extend(preview_lines(p, body_height));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "enter re-attaches  ·  d closes the window",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            _ => {
                lines.push(Line::from(Span::styled(
                    format!("attach into window `{}`", w.name),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "→ tmux attach -t {}:{}",
                        app.terminals().session_name,
                        w.index
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    detach_hint,
                    Style::default().fg(Color::DarkGray),
                )));
            }
        },
        Some(crate::app::TerminalsRow::NewSentinel) => {
            lines.push(Line::from(Span::styled(
                "open a new shell",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("→ exec $SHELL in {}", app.project_root().display()),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                detach_hint,
                Style::default().fg(Color::DarkGray),
            )));
        }
        None => {}
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Trim a captured pane's visible content to the last `max_rows`
/// non-empty lines (drops a leading run of blank rows so the user
/// sees content rather than empty space). Falls back to the
/// original tail when the pane is small.
fn preview_lines(captured: &[String], max_rows: usize) -> Vec<Line<'static>> {
    if captured.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    let trimmed: &[String] = match captured.iter().rposition(|l| !l.trim().is_empty()) {
        Some(last) => &captured[..=last],
        None => captured,
    };
    let start = trimmed.len().saturating_sub(max_rows);
    trimmed[start..]
        .iter()
        .map(|s| Line::from(Span::raw(s.clone())))
        .collect()
}

/// Real Diff body: file list sidebar + diff body right pane.
fn render_diff_placeholder(app: &App, frame: &mut Frame, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);
    render_diff_files(app, frame, body[0]);
    render_diff_body(app, frame, body[1]);
}

fn render_diff_files(app: &App, frame: &mut Frame, area: Rect) {
    use crate::app::DiffStatus;
    let diff = app.diff();
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "changes",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", diff.files.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = panel_block_titled(title);

    if !diff.loaded {
        let body = Paragraph::new(Line::from(Span::styled(
            "  loading…",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(body, area);
        return;
    }
    if let Some(err) = diff.error.as_ref() {
        let body = Paragraph::new(vec![
            Line::from(Span::styled(
                "  error".to_string(),
                Style::default().fg(Color::Red),
            )),
            Line::from(Span::styled(
                format!("  {err}"),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block);
        frame.render_widget(body, area);
        return;
    }
    if diff.files.is_empty() {
        let body = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no changes — working tree clean",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  press r to refresh",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block);
        frame.render_widget(body, area);
        return;
    }

    let items: Vec<ListItem> = diff
        .files
        .iter()
        .enumerate()
        .map(|(idx, f)| {
            let row_style = if idx == diff.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let letter_style = match f.status {
                DiffStatus::Modified => Style::default().fg(Color::Yellow),
                DiffStatus::Added | DiffStatus::Untracked => Style::default().fg(Color::Green),
                DiffStatus::Deleted => Style::default().fg(Color::Red),
                DiffStatus::Renamed => Style::default().fg(Color::Cyan),
                DiffStatus::Other => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", f.status.letter()), letter_style),
                Span::styled(f.path.clone(), row_style),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_widget(list, area);
}

fn render_diff_body(app: &App, frame: &mut Frame, area: Rect) {
    let title_text = match app.diff_selected_file() {
        Some(f) => format!("  diff · {}", f.path),
        None => "  diff".into(),
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title_text,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));

    let lines: Vec<Line<'static>> = match app.diff_selected_file() {
        Some(f) => match app.diff_cache_for(&f.path) {
            Some(diff_lines) => {
                let max = area.height.saturating_sub(2) as usize;
                let total = diff_lines.len();
                let start = total.saturating_sub(max);
                diff_lines
                    .iter()
                    .skip(start)
                    .map(render_diff_line)
                    .collect()
            }
            None => vec![Line::from(Span::styled(
                "loading diff…",
                Style::default().fg(Color::DarkGray),
            ))],
        },
        None => vec![Line::from(Span::styled(
            "select a file on the left",
            Style::default().fg(Color::DarkGray),
        ))],
    };

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_diff_line(line: &crate::app::DiffLine) -> Line<'static> {
    use crate::app::DiffLineKind;
    let style = match line.kind {
        DiffLineKind::Added => Style::default().fg(Color::Green),
        DiffLineKind::Removed => Style::default().fg(Color::Red),
        DiffLineKind::Hunk => Style::default().fg(Color::Cyan),
        DiffLineKind::Header => Style::default().fg(Color::DarkGray),
        DiffLineKind::Context => Style::default(),
    };
    Line::from(Span::styled(line.text.clone(), style))
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
    let mut container = Vec::new();
    let mut services = Vec::new();
    let mut watchers = Vec::new();
    let mut commands = Vec::new();
    for item in app.items() {
        match item.kind {
            ItemKind::Container => container.push(item),
            ItemKind::Service => services.push(item),
            ItemKind::Watcher => watchers.push(item),
            ItemKind::Recipe | ItemKind::Script => commands.push(item),
        }
    }
    let mut out = Vec::new();
    if !container.is_empty() {
        out.push(SidebarGroup {
            label: "container",
            items: container,
        });
    }
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
        // Container reuses the service dot — same "is this thing
        // alive" mental model.
        ItemKind::Container | ItemKind::Service => "●",
        ItemKind::Watcher => "◇",
        // Same glyph for recipes and scripts — they share the
        // "commands" sidebar group. Kind is still distinguishable in
        // the detail pane and palette tags.
        ItemKind::Recipe | ItemKind::Script => "▸",
    }
}

fn item_indicator_style(app: &App, item: &Item) -> Style {
    match item.kind {
        ItemKind::Container => run_indicator_style(app.lifecycle_run()),
        ItemKind::Service => service_indicator_style(app, &item.name),
        ItemKind::Watcher => watcher_indicator_style(app, &item.name),
        ItemKind::Recipe | ItemKind::Script => {
            run_indicator_style(app.run_for(item.kind, &item.name))
        }
    }
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
fn run_indicator_style(run: Option<&RunState>) -> Style {
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

fn render_right_pane(app: &App, frame: &mut Frame, area: Rect) {
    // Right pane is purely selection-driven: services tail their
    // pane, watchers their buffer, recipes / scripts their RunState,
    // and the synthetic container row hosts compose lifecycle output.
    // No bottom split — the lifecycle output lives in its own row
    // now, navigated to like anything else.
    render_focused_buffer(app, frame, area);
}

/// Paint the buffer for whatever's selected. Each kind has its own
/// source.
fn render_focused_buffer(app: &App, frame: &mut Frame, area: Rect) {
    let Some(item) = app.selected_item() else {
        let block = panel_block(" output ");
        frame.render_widget(block, area);
        return;
    };
    match item.kind {
        ItemKind::Container => match app.lifecycle_run() {
            Some(run) => render_run_buffer(run, frame, area),
            None => render_idle_container(item, frame, area),
        },
        ItemKind::Service => {
            if let Some(service) = app.selected_service() {
                render_service_logs(service, frame, area);
            }
        }
        ItemKind::Watcher => {
            if let Some(watcher) = app.selected_watcher() {
                render_watcher(watcher, frame, area);
            }
        }
        ItemKind::Recipe | ItemKind::Script => match app.selected_run() {
            Some(run) => render_run_buffer(run, frame, area),
            None => render_idle_buffer(item, frame, area),
        },
    }
}

/// Placeholder for the container row when no lifecycle action has
/// run yet. Tells the user how to wake the row up.
fn render_idle_container(item: &Item, frame: &mut Frame, area: Rect) {
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("  idle".to_string(), Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "no lifecycle action has run yet",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "U up all · D down all · R restart all · S stop all",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(block);
    frame.render_widget(body, area);
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
        ItemKind::Watcher => app
            .selected_watcher()
            .map(|w| (None, watcher_kv_lines(w)))
            .unwrap_or_default(),
        // Container / services don't have config-level descriptions.
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
        ItemKind::Container => "container",
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

    // Same padding as recipe / lifecycle output panes so all three
    // sit in the same visual frame. Compose's own `<svc-N> |`
    // prefix is short and stays readable inside the gutter.
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));

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
    // Watcher meta (recipe / globs / debounce) lives in the details
    // panel under the sidebar. The output pane is purely the buffer
    // from the most recent run, with a placeholder when nothing has
    // run yet — matching the recipe / script / lifecycle pane shape.
    let title = watcher_pane_title(watcher);
    let block = panel_block_titled(title).padding(Padding::new(2, 1, 1, 0));

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

fn render_worktree_switcher(switcher: &crate::app::WorktreeSwitcher, frame: &mut Frame) {
    // Two sub-views: list (default) and the create form. They share
    // the same outer block; the form is taller because it has two
    // input rows plus a hint and an error line.
    if let Some(form) = switcher.creating.as_ref() {
        render_switcher_form(form, frame);
    } else {
        render_switcher_list(switcher, frame);
    }
}

fn render_switcher_list(switcher: &crate::app::WorktreeSwitcher, frame: &mut Frame) {
    let total_rows = switcher.total_rows();
    let height = (total_rows as u16 + 4).min(20);
    let area = centered_rect(frame.area(), 60, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "switch worktree",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(total_rows + 2);
    for (idx, row) in switcher.entries.iter().enumerate() {
        let prefix = if idx == switcher.selected {
            "▶ "
        } else {
            "  "
        };
        let row_style = if idx == switcher.selected {
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let current_marker = if row.is_current {
            Span::styled(" ●", Style::default().fg(Color::Green))
        } else {
            Span::raw("  ")
        };
        let branch_label = row
            .branch
            .clone()
            .unwrap_or_else(|| "<detached>".to_string());
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(format!("{branch_label:<24}"), row_style),
            current_marker,
            Span::raw("  "),
            Span::styled(
                row.path.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    // Sentinel "+ new worktree" row.
    let new_idx = switcher.new_row_index();
    let prefix = if switcher.selected == new_idx {
        "▶ "
    } else {
        "  "
    };
    let row_style = if switcher.selected == new_idx {
        Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ACCENT)
    };
    lines.push(Line::from(vec![
        Span::styled(prefix.to_string(), row_style),
        Span::styled("+ new worktree".to_string(), row_style),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ nav · enter switch · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_switcher_form(form: &crate::app::NewWorktreeForm, frame: &mut Frame) {
    use crate::app::NewFormField;
    let height = if form.error.is_some() { 11 } else { 9 };
    let area = centered_rect(frame.area(), 60, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "new worktree",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let path_focus = matches!(form.focus, NewFormField::Path);
    let branch_focus = matches!(form.focus, NewFormField::Branch);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(8);
    lines.push(field_row("path", &form.path_input, path_focus));
    lines.push(field_row("branch", &form.branch_input, branch_focus));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "tab toggle · enter create · esc back",
        Style::default().fg(Color::DarkGray),
    )));
    if let Some(err) = form.error.as_ref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn field_row(label: &'static str, value: &str, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let cursor_span = if focused {
        Span::styled("█", Style::default().fg(ACCENT))
    } else {
        Span::raw("")
    };
    Line::from(vec![
        Span::styled(format!("{label:<8}"), label_style),
        Span::styled(value.to_string(), value_style),
        cursor_span,
    ])
}

fn render_args_prompt(prompt: &crate::app::ArgsPrompt, frame: &mut Frame) {
    let area = centered_rect(frame.area(), 60, 7);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("args for `{}`", prompt.item_name),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let body = vec![
        Line::from(vec![
            Span::styled("> ", Style::default().fg(ACCENT)),
            Span::styled(
                prompt.input.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            // Block-character cursor at the end so users see where input goes.
            Span::styled("█", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "↵ run · esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(body).block(block), area);
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

/// Hint set for the active view. Phases 3 and 4 fill in their own
/// view-specific hints; for now the placeholder views just show
/// the global view-switch keys.
fn view_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.view() {
        crate::app::View::ControlCenter => control_center_hints(app),
        crate::app::View::Terminals => vec![
            ("↑↓", "nav"),
            ("enter", "attach"),
            ("n", "new"),
            ("d", "delete"),
            ("C", "control"),
            ("G", "diff"),
            ("W", "worktree"),
            ("q", "quit"),
        ],
        crate::app::View::Diff => vec![
            ("↑↓", "nav"),
            ("r", "refresh"),
            ("C", "control"),
            ("T", "terminals"),
            ("W", "worktree"),
            ("q", "quit"),
        ],
    }
}

fn control_center_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let mode_palette = app.mode() == Mode::Palette;

    let mut hints: Vec<(&str, &str)> = vec![("↑↓", "nav")];
    if !mode_palette {
        // Enter label adapts to the row kind so users see what
        // pressing it will do without consulting docs.
        let enter_label = match app.selected_item().map(|i| i.kind) {
            Some(crate::app::ItemKind::Container) => "up all",
            Some(crate::app::ItemKind::Service) => "attach",
            Some(crate::app::ItemKind::Recipe | crate::app::ItemKind::Script) => "run",
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
    }
    hints.push(("/  :", "palette"));
    hints.push(("q", "quit"));
    hints
}

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

    let hints = view_hints(app);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(hints.len() * 4 + 4);
    // Leading view tag — `[control]` / `[terminals]` / `[diff]`.
    // Anchors the user to which view their hints apply to.
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("[{}]", app.view().tag()),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw("  "));
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
        app.set_tmux_available(true);
        app.switch_view(crate::app::View::Terminals);
        app.terminals_set_windows(vec![crate::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
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
            scaffl_config::parse_str(
                r#"
                [project]
                name = "tuitest"

                [containers]
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
        app.set_tmux_available(true);
        app.switch_view(crate::app::View::Terminals);
        app.terminals_set_windows(vec![crate::app::TmuxWindow {
            index: 0,
            name: "zsh".into(),
            cwd: None,
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
