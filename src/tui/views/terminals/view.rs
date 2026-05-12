//! Render for the Terminals view.
//!
//! Entry point: [`render`]. Falls back to a "tmux missing" splash
//! when the probe at [`crate::tui::app::App::request_tmux_probe`]
//! came back negative. Otherwise it splits the area into the
//! sidebar + info / preview right column, mirroring the control
//! center's geometry.

use crate::tui::app::App;
use crate::tui::ui::{
    SIDEBAR_RATIO, accent_of, kv, panel_block, panel_block_titled, service_indicator_style,
    split_info_output,
};
use crate::tui::views::terminals::state::{TerminalsRow, TmuxWindow};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Padding, Paragraph};

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
    use crate::tui::shared::sidebar_layout::{JoinPosition, SidebarGroup, render_grouped_sidebar};

    let rows = app.terminals_rows();
    let selected = app.terminals().selected.min(rows.len().saturating_sub(1));
    let accent = accent_of(app);

    // Bucket the global rows into services-on-top + windows+sentinel
    // below. The flat-index order matches `terminals_rows()` so a
    // global index → rect lookup is a straight Vec subscript.
    let mut svc_items: Vec<ListItem> = Vec::new();
    let mut svc_selected: Option<usize> = None;
    let mut term_items: Vec<ListItem> = Vec::new();
    let mut term_selected: Option<usize> = None;
    for (global_idx, row) in rows.iter().enumerate() {
        match row {
            TerminalsRow::Service(name) => {
                let local_idx = svc_items.len();
                if global_idx == selected {
                    svc_selected = Some(local_idx);
                }
                let glyph_style = service_indicator_style(app, name);
                svc_items.push(ListItem::new(Line::from(vec![
                    Span::styled("● ", glyph_style),
                    Span::raw(name.clone()),
                ])));
            }
            TerminalsRow::Window(w) => {
                let local_idx = term_items.len();
                if global_idx == selected {
                    term_selected = Some(local_idx);
                }
                term_items.push(ListItem::new(window_row_line(w)));
            }
            TerminalsRow::NewSentinel => {
                let local_idx = term_items.len();
                if global_idx == selected {
                    term_selected = Some(local_idx);
                }
                term_items.push(ListItem::new(Line::from(Span::styled(
                    "+ new shell",
                    Style::default().fg(accent),
                ))));
            }
        }
    }

    let mut groups: Vec<SidebarGroup<'_>> = Vec::new();
    let mut joins: Vec<JoinPosition> = Vec::new();
    if !svc_items.is_empty() {
        groups.push(SidebarGroup {
            label: "services",
            items: svc_items,
            selected_local: svc_selected,
        });
        joins.push(JoinPosition::Standalone);
    }
    // The "terminals" group always renders even when empty — the
    // `+ new shell` sentinel is in `rows`, so `term_items` always
    // has at least one entry. We still guard with `is_empty()` for
    // robustness against future row-list changes.
    if !term_items.is_empty() {
        // Subtract 1 for the synthetic `+ new shell` sentinel so the
        // count reflects user-created windows.
        let label = "terminals";
        let _ = label; // suppress lint when items are empty
        groups.push(SidebarGroup {
            label,
            items: term_items,
            selected_local: term_selected,
        });
        joins.push(JoinPosition::Standalone);
    }

    let mut rects = app.terminals().row_rects.borrow_mut();
    render_grouped_sidebar(frame, area, &groups, &joins, accent, &mut rects, None);
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

fn render_preview(preview: Option<&Vec<String>>, accent: Color, frame: &mut Frame, area: Rect) {
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
