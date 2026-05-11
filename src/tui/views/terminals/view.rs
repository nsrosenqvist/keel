//! Render for the Terminals view.
//!
//! Entry point: [`render`]. Falls back to a "tmux missing" splash
//! when the probe at [`crate::tui::views::terminals::tmux::ensure_tmux_probed`]
//! came back negative. Otherwise it splits the area into the
//! sidebar + info / preview right column, mirroring the control
//! center's geometry.

use crate::tui::app::App;
use crate::tui::ui::{
    SELECTION_BG, SELECTION_FG, SIDEBAR_RATIO, accent_of, kv, panel_block, panel_block_titled,
    service_indicator_style, split_info_output,
};
use crate::tui::views::terminals::state::{TerminalsRow, TmuxWindow};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Padding, Paragraph};

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(false) = app.terminals().tmux_available {
        render_tmux_missing(accent_of(app), frame, area);
        return;
    }
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);
    render_sidebar(app, frame, body[0]);
    render_info(app, frame, body[1]);
}

fn render_tmux_missing(accent: Color, frame: &mut Frame, area: Rect) {
    let block = panel_block(" terminals ", accent);
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
            "  then press T again, or restart keel.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(block);
    frame.render_widget(body, area);
}

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let rows = app.terminals_rows();
    let selected = app.terminals().selected.min(rows.len().saturating_sub(1));

    // Split the sidebar into two stacked groups: services on top,
    // terminals + sentinel below. Group sizing mirrors the control
    // center's per-group constraints.
    let services_count = rows
        .iter()
        .filter(|r| matches!(r, TerminalsRow::Service(_)))
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

    let accent = accent_of(app);
    // Selection chrome: muted dark grey background with white text.
    // Shared across views so the "this is the selected row"
    // affordance reads consistently. The view's accent appears in
    // the row's foreground content (e.g. group titles) — keeping
    // the highlight neutral lets it not compete for attention.
    let highlight = Style::default()
        .fg(SELECTION_FG)
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD);

    // Services group
    if services_count > 0 {
        let mut svc_items: Vec<ListItem> = Vec::new();
        let mut svc_selected: Option<usize> = None;
        for (local_idx, (global_idx, row)) in rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, TerminalsRow::Service(_)))
            .enumerate()
        {
            let TerminalsRow::Service(name) = row else {
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
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
            TerminalsRow::Service(_) => continue,
            TerminalsRow::Window(w) => {
                if global_idx == selected {
                    term_selected = Some(term_items.len());
                }
                term_items.push(ListItem::new(window_row_line(w)));
            }
            TerminalsRow::NewSentinel => {
                if global_idx == selected {
                    term_selected = Some(term_items.len());
                }
                term_items.push(ListItem::new(Line::from(Span::styled(
                    "+ new shell",
                    Style::default().fg(accent),
                ))));
            }
        }
    }
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "terminals",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
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

    // Per-row rects keyed by the global index into `app.terminals_rows()`.
    // Services occupy the top group; windows + sentinel occupy the
    // bottom group (or the only group when no services exist).
    let mut rects = app.terminals().row_rects.borrow_mut();
    rects.clear();
    rects.resize(rows.len(), Rect::default());
    let svc_area = if services_count > 0 {
        Some(areas[0])
    } else {
        None
    };
    let mut svc_local = 0usize;
    let mut term_local = 0usize;
    for (global_idx, row) in rows.iter().enumerate() {
        let (group_area, local_idx) = match row {
            TerminalsRow::Service(_) => {
                let Some(a) = svc_area else { continue };
                let l = svc_local;
                svc_local += 1;
                (a, l)
            }
            TerminalsRow::Window(_) | TerminalsRow::NewSentinel => {
                let l = term_local;
                term_local += 1;
                (area_for_terms, l)
            }
        };
        let inner_y = group_area.y.saturating_add(1);
        let inner_h = group_area.height.saturating_sub(2);
        if (local_idx as u16) >= inner_h {
            continue;
        }
        rects[global_idx] = Rect {
            x: group_area.x.saturating_add(1),
            y: inner_y + local_idx as u16,
            width: group_area.width.saturating_sub(2),
            height: 1,
        };
    }
}

/// Render one window row in the terminals sidebar. Tmux's
/// automatic-rename keeps `name` in sync with the running command
/// (`zsh`, `vim`, …); when we have a `cwd` populated, we append
/// it for the kind of "tab title" feel a real terminal would
/// show. The cwd is collapsed against $HOME (`~`) to fit narrow
/// sidebars.
///
/// A pending bell (tmux's `#{window_bell_flag}`) swaps the leading
/// diamond for a yellow filled dot — the indicator we surface for
/// "this window wants attention" (coding agents emit BEL when
/// they're waiting on input). The flag clears on attach.
pub(crate) fn window_row_line(w: &TmuxWindow) -> Line<'static> {
    let (glyph, glyph_style) = if w.has_bell {
        ("● ", Style::default().fg(Color::Yellow))
    } else {
        ("◇ ", Style::default().fg(Color::DarkGray))
    };
    let mut spans = vec![
        Span::styled(glyph, glyph_style),
        Span::styled(
            format!("{}: ", w.index),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(w.name.clone()),
    ];
    // Devcontainer windows: replace the host-side `cwd` (which is
    // just the docker client's pwd) with the in-container
    // workspace folder tmux recorded via `@keel_workspace`. Falls
    // back to the regular `cwd` for host shells / service windows /
    // anything else keel didn't tag.
    if let Some(workspace) = w.workspace.as_deref() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            workspace.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    } else if let Some(cwd) = w.cwd.as_deref() {
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

/// Right column for the terminals view: info panel on top
/// (selected row's identity + the command we'd run on attach +
/// detach hint), preview panel below (tmux capture-pane output, or
/// a fallback "press enter to attach" placeholder when there's
/// nothing to show yet).
fn render_info(app: &App, frame: &mut Frame, area: Rect) {
    let rows = app.terminals_rows();
    let selected_row = rows.get(app.terminals().selected);

    let preview_index = match selected_row {
        Some(TerminalsRow::Window(w)) => Some(w.index),
        Some(TerminalsRow::Service(name)) => app
            .terminals()
            .windows
            .iter()
            .find(|w| w.name == format!("svc:{name}"))
            .map(|w| w.index),
        _ => None,
    };
    let preview = preview_index.and_then(|i| app.terminals().preview(i));

    let info_body = build_info_body(app, selected_row);
    let [info_area, preview_area] = split_info_output(area, info_body.len());

    let info_title = info_title(app, selected_row);
    let info_block = panel_block_titled(info_title).padding(Padding::horizontal(2));
    frame.render_widget(Paragraph::new(info_body).block(info_block), info_area);

    render_preview(preview, accent_of(app), frame, preview_area);
}

fn info_title(app: &App, selected_row: Option<&TerminalsRow>) -> Line<'static> {
    let label = match selected_row {
        Some(TerminalsRow::Window(w)) => w.name.clone(),
        Some(TerminalsRow::Service(name)) => format!("svc:{name}"),
        Some(TerminalsRow::NewSentinel) => "+ new shell".into(),
        None => "tmux".into(),
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("  session: {}", app.terminals().session_name),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
    ])
}

fn build_info_body(app: &App, selected_row: Option<&TerminalsRow>) -> Vec<Line<'static>> {
    let detach_hint = "ctrl+b d returns to keel";
    let mut lines: Vec<Line<'static>> = Vec::new();
    match selected_row {
        Some(TerminalsRow::Service(name)) => {
            lines.push(kv(
                "command",
                &format!("docker compose exec -it {name} $SHELL"),
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "enter re-attaches  ·  d closes the window",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                detach_hint.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        Some(TerminalsRow::Window(w)) => {
            if let Some(cwd) = w.cwd.as_deref() {
                lines.push(kv("cwd", &collapse_home(cwd)));
            }
            lines.push(kv(
                "attach",
                &format!(
                    "tmux attach -t {}:{}",
                    app.terminals().session_name,
                    w.index
                ),
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "enter re-attaches  ·  d closes the window",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                detach_hint.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        Some(TerminalsRow::NewSentinel) => {
            lines.push(kv(
                "command",
                &format!("exec $SHELL in {}", app.project_root().display()),
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "enter opens a new shell",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                detach_hint.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        None => {}
    }
    lines
}

fn render_preview(
    preview: Option<&Vec<String>>,
    accent: Color,
    frame: &mut Frame,
    area: Rect,
) {
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "preview",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));

    let body: Vec<Line<'static>> = match preview {
        Some(p) if !p.is_empty() => {
            let body_height = area.height.saturating_sub(2) as usize;
            preview_lines(p, body_height)
        }
        _ => vec![Line::from(Span::styled(
            "press enter to attach — preview appears after the first attach",
            Style::default().fg(Color::DarkGray),
        ))],
    };

    frame.render_widget(Paragraph::new(body).block(block), area);
}

/// Trim a captured pane's visible content to the last `max_rows`
/// non-empty lines (drops a leading run of blank rows so the user
/// sees content rather than empty space). Falls back to the
/// original tail when the pane is small.
fn preview_lines(captured: &[String], max_rows: usize) -> Vec<Line<'static>> {
    if captured.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    // Trim trailing blank rows but leave leading blanks alone so the
    // capture's vertical alignment is preserved.
    let trimmed: &[String] = match captured.iter().rposition(|l| !l.trim().is_empty()) {
        Some(last) => &captured[..=last],
        None => captured,
    };
    let start = trimmed.len().saturating_sub(max_rows);
    trimmed[start..]
        .iter()
        .map(|s| crate::tui::ansi::ansi_to_line(s, Style::default()))
        .collect()
}
