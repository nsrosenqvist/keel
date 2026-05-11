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
use crate::tui::dialogs::args_prompt::ArgsPrompt;
use crate::tui::dialogs::confirm::ConfirmDialog;
use crate::tui::dialogs::switcher::{NewFormField, NewWorktreeForm};
use crate::tui::palette::Palette;
use crate::tui::runner::{CapturedLine, RunState};
use crate::tui::services::ServicePane;
use crate::tui::views::control_center::state::{Item, ItemKind};
use crate::tui::views::diff::state::{
    BodyMode, DiffFile, DiffFocus, DiffLine, DiffLineKind, DiffStatus, ReadLine, ReadLineKind,
};
use crate::tui::views::terminals::state::{TerminalsRow, TmuxWindow};
use crate::tui::watchers::{WatcherPane, WatcherState};
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

const SIDEBAR_RATIO: u16 = 28;
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

fn accent_of(app: &App) -> Color {
    view_accent(app.view())
}

/// Selection highlight chrome — same across all views so the
/// "selected row" affordance reads consistently. Subtle dim grey
/// background pairs with the active accent for the row's text;
/// avoids the loud cyan-bg / black-fg contrast we used before.
const SELECTION_BG: Color = Color::Indexed(238);
const SELECTION_FG: Color = Color::Indexed(255);

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
        crate::tui::app::View::Terminals => render_terminals_placeholder(app, frame, outer[1]),
        crate::tui::app::View::Diff => render_diff_placeholder(app, frame, outer[1]),
    }

    render_status(app, frame, outer[2]);

    let accent = accent_of(app);
    if app.mode() == Mode::Palette
        && let Some(palette) = app.palette()
    {
        render_palette(app, palette, accent, frame);
    }
    if app.mode() == Mode::Confirm
        && let Some(dialog) = app.confirm_dialog()
    {
        render_confirm_modal(app, dialog, accent, frame);
    }
    if app.mode() == Mode::ArgsPrompt
        && let Some(prompt) = app.args_prompt()
    {
        render_args_prompt(prompt, accent, frame);
    }
    if app.mode() == Mode::WorktreeSwitcher
        && let Some(switcher) = app.switcher()
    {
        render_worktree_switcher(switcher, accent, frame);
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

/// Real Terminals body: tmux-backed sidebar + info panel.
fn render_terminals_placeholder(app: &App, frame: &mut Frame, area: Rect) {
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
    render_terminals_sidebar(app, frame, body[0]);
    render_terminals_info(app, frame, body[1]);
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

fn render_terminals_sidebar(app: &App, frame: &mut Frame, area: Rect) {
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
            TerminalsRow::Window(_)
            | TerminalsRow::NewSentinel => {
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
fn window_row_line(w: &TmuxWindow) -> Line<'static> {
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
fn render_terminals_info(app: &App, frame: &mut Frame, area: Rect) {
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
    let preview = preview_index.and_then(|i| app.terminals_preview(i));

    let info_body = build_terminals_info_body(app, selected_row);
    let [info_area, preview_area] = split_info_output(area, info_body.len());

    let info_title = terminals_info_title(app, selected_row);
    let info_block = panel_block_titled(info_title).padding(Padding::horizontal(2));
    frame.render_widget(Paragraph::new(info_body).block(info_block), info_area);

    render_terminals_preview(preview, accent_of(app), frame, preview_area);
}

fn terminals_info_title(
    app: &App,
    selected_row: Option<&TerminalsRow>,
) -> Line<'static> {
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

fn build_terminals_info_body(
    app: &App,
    selected_row: Option<&TerminalsRow>,
) -> Vec<Line<'static>> {
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

fn render_terminals_preview(
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

/// Real Diff body: file list sidebar + diff body right pane.
fn render_diff_placeholder(app: &App, frame: &mut Frame, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(area);
    // Banner sits above the file list because it summarises the
    // whole comparison (branch → trunk, total churn), not the
    // currently-selected file's diff. Visual hierarchy: scope on
    // the left, contents on the right.
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(body[0]);
    render_diff_header(app, frame, left[0]);
    // Stash the file-list and body rects so the mouse handler can
    // hit-test wheel/click events. Updated every frame so a window
    // resize is reflected on the next click.
    app.diff().files_rect.set(Some(left[1]));
    app.diff().body_rect.set(Some(body[1]));
    render_diff_files(app, frame, left[1]);
    render_diff_body(app, frame, body[1]);
}

/// Bordered block whose border colour changes with focus: bright
/// accent + bold when this pane has the keyboard focus, dim grey
/// otherwise. Mirrors the visual language used elsewhere; users see
/// a single "active" frame at a time. Bold on the focused border
/// gives a second cue (weight, not just colour) so the active pane
/// is unmistakable even on terminals that wash out 256-colour
/// indices.
fn diff_panel_block(title: Line<'static>, focused: bool, accent: Color) -> Block<'static> {
    let border = if focused {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let border_type = if focused {
        BorderType::Thick
    } else {
        BorderType::Rounded
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border)
}

/// Comparison banner — sits above the file list and tells the
/// user the scope of the diff: `<branch> → <trunk> · <short-sha>`
/// on row 1, `<n> files · +X −Y` on row 2. A bordered panel like
/// the file list and body so the three visually stack as one unit.
fn render_diff_header(app: &App, frame: &mut Frame, area: Rect) {
    let diff = app.diff();
    let accent = accent_of(app);
    let trunk_label = diff.trunk.clone().unwrap_or_else(|| "HEAD".into());
    let branch_label = diff
        .branch
        .clone()
        .or_else(|| app.branch().map(str::to_string))
        .unwrap_or_else(|| "(detached)".into());

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "compare",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let block = panel_block_titled(title).padding(Padding::horizontal(2));

    let mut row1: Vec<Span<'static>> = vec![
        Span::styled(
            branch_label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" → ", Style::default().fg(Color::DarkGray)),
        Span::styled(trunk_label, Style::default().add_modifier(Modifier::BOLD)),
    ];
    if let Some(sha) = diff.anchor_short.as_ref() {
        row1.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        row1.push(Span::styled(
            sha.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let n = diff.files.len();
    let row2 = vec![
        Span::styled(
            format!("{n} {}", if n == 1 { "file" } else { "files" }),
            Style::default().fg(Color::White).dim(),
        ),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("+{}", diff.additions_total),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("−{}", diff.deletions_total),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ];

    let body = Paragraph::new(vec![Line::from(row1), Line::from(row2)])
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

fn render_diff_files(app: &App, frame: &mut Frame, area: Rect) {
    let accent = accent_of(app);
    let diff = app.diff();
    let focused = diff.focus == DiffFocus::Files;
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "changes",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", diff.files.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = diff_panel_block(title, focused, accent);

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
        let trunk_hint = diff
            .trunk
            .as_deref()
            .map(|t| format!("  no changes vs {t}"))
            .unwrap_or_else(|| "  no changes — working tree clean".into());
        let body = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                trunk_hint,
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

    // Right-align the churn column — pad inner area so paths
    // truncate gracefully on narrow terminals before the churn.
    let inner_w = area.width.saturating_sub(2) as usize; // borders
    let churn_w = 14usize; // ` +999 -999 `, generous
    let path_w = inner_w.saturating_sub(churn_w + 4); // status letter + spaces

    let items: Vec<ListItem> = diff
        .files
        .iter()
        .map(|f| {
            let letter_style = match f.status {
                DiffStatus::Modified => Style::default().fg(Color::Yellow),
                DiffStatus::Added | DiffStatus::Untracked => Style::default().fg(Color::Green),
                DiffStatus::Deleted => Style::default().fg(Color::Red),
                DiffStatus::Renamed => Style::default().fg(Color::Indexed(75)),
                DiffStatus::Other => Style::default().fg(Color::DarkGray),
            };
            // Path elision: keep the tail (filename) over the head
            // when truncating — `…/tui/src/ui.rs` is more useful
            // than `crates/keel-tui/src/u…`.
            let path = elide_left(&f.path, path_w);
            let path_pad = path_w.saturating_sub(path.chars().count());
            let churn = if f.binary {
                vec![Span::styled("  bin", Style::default().fg(Color::DarkGray))]
            } else if f.status == DiffStatus::Untracked {
                // Untracked files can't have deletions by definition,
                // but we still render the `−0` column so the grid
                // aligns with tracked rows. Dim it so the eye lands
                // on the meaningful `+N` first.
                vec![
                    Span::styled(
                        format!("+{}", f.additions),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(" "),
                    Span::styled("−0", Style::default().fg(Color::DarkGray)),
                ]
            } else {
                vec![
                    Span::styled(
                        format!("+{}", f.additions),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(" "),
                    Span::styled(format!("−{}", f.deletions), Style::default().fg(Color::Red)),
                ]
            };
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(format!(" {} ", f.status.letter()), letter_style),
                Span::raw(path),
                Span::raw(" ".repeat(path_pad)),
            ];
            spans.extend(churn);
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(SELECTION_FG)
                .bg(SELECTION_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default();
    state.select(Some(diff.selected));
    frame.render_stateful_widget(list, area, &mut state);

    // Capture per-file row rects for mouse hit-testing. ratatui's
    // List has auto-scrolled `state.offset()` so the selection stays
    // in view; we use that offset to map file index → on-screen row.
    // Items outside the visible window get a zero-area sentinel —
    // `rect_contains` filters them out at hit-test time.
    let inner_x = area.x.saturating_add(1);
    let inner_y = area.y.saturating_add(1);
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    let offset = state.offset();
    let mut rects = diff.file_row_rects.borrow_mut();
    rects.clear();
    rects.reserve(diff.files.len());
    for i in 0..diff.files.len() {
        if i < offset {
            rects.push(Rect::default());
            continue;
        }
        let row = (i - offset) as u16;
        if row >= inner_h {
            rects.push(Rect::default());
            continue;
        }
        rects.push(Rect {
            x: inner_x,
            y: inner_y + row,
            width: inner_w,
            height: 1,
        });
    }
}

/// Truncate `s` from the left (head), keeping the tail. Adds an
/// ellipsis prefix when truncation happens. Operates on chars so
/// non-ASCII paths don't get sliced mid-codepoint.
fn elide_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".into();
    }
    let skip = count - (max - 1);
    let tail: String = s.chars().skip(skip).collect();
    format!("…{tail}")
}

fn render_diff_body(app: &App, frame: &mut Frame, area: Rect) {
    let accent = accent_of(app);
    let diff = app.diff();
    let focused = diff.focus == DiffFocus::Body;
    let mode = diff.body_mode;
    let mode_label = match mode {
        BodyMode::Diff => "diff",
        BodyMode::Read => "read",
    };
    let title_text = match app.diff_selected_file() {
        Some(f) => format!("{mode_label} · {}", f.path),
        None => mode_label.into(),
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title_text,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let block = diff_panel_block(title, focused, accent).padding(Padding::horizontal(1));

    // Stash the body's inner viewport height so the key handler
    // can size half-pages and clamp G to the bottom. -2 for the
    // top/bottom borders.
    let inner_height = area.height.saturating_sub(2);
    diff.body_height.set(inner_height);
    // Inner width = area width minus 2 borders minus 2 cols of
    // `Padding::horizontal(1)`. Used by the horizontal-scroll clamp
    // so panning stops when the longest row's right edge meets the
    // viewport's right edge.
    diff.body_width.set(area.width.saturating_sub(4));

    let Some(file) = app.diff_selected_file() else {
        let body = Paragraph::new(Line::from(Span::styled(
            "select a file on the left",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(body, area);
        return;
    };

    match mode {
        BodyMode::Diff => render_body_diff(app, frame, area, file, block, inner_height),
        BodyMode::Read => render_body_read(app, frame, area, file, block, inner_height),
    }
}

fn render_body_diff(
    app: &App,
    frame: &mut Frame,
    area: Rect,
    file: &DiffFile,
    block: Block<'static>,
    inner_height: u16,
) {
    let diff = app.diff();
    let lines = match app.diff_cache_for(&file.path) {
        Some(l) => l,
        None => {
            let body = Paragraph::new(Line::from(Span::styled(
                "loading diff…",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block);
            frame.render_widget(body, area);
            return;
        }
    };

    // Gutter width: max line number across this file. Use string
    // length so 4-digit lines aren't squashed against the sigil.
    let max_lineno = lines
        .iter()
        .filter_map(|l| l.new_lineno.map(u64::from).or(l.old_lineno.map(u64::from)))
        .max()
        .unwrap_or(0);
    let gutter_w = max_lineno.to_string().len().max(1);

    // In non-wrap mode pad tinted rows to `max(pane_width,
    // longest_line)`. The pane-width floor extends bg out to the
    // right edge for short lines; the longest-line ceiling keeps
    // bg continuous as the user horizontally scrolls past the pane
    // edge — without it, scrolling reveals plain terminal cells to
    // the right of short tinted rows. Wrap mode skips padding so
    // trailing whitespace doesn't push lines onto an extra wrapped
    // row.
    let pad_to = if diff.wrap {
        None
    } else {
        let pane = area.width.saturating_sub(4) as usize;
        let longest = lines
            .iter()
            .map(|l| diff_line_visual_width(l, gutter_w))
            .max()
            .unwrap_or(0);
        Some(pane.max(longest))
    };

    let scroll = diff.body_scroll.get(&file.path).copied().unwrap_or(0);
    // `diff_body_h_scroll` clamps to 0 when wrap is on — wrap mode
    // has no horizontal axis, and a stale map entry must not bleed
    // into rendering. Cast saturates so a >u16::MAX offset (which
    // would be a bug elsewhere) doesn't wrap around.
    let h_scroll: u16 = app.diff_body_h_scroll().min(u16::MAX as usize) as u16;
    if diff.wrap {
        // Wrap mode: render every line; let Paragraph handle the
        // scroll. Slower for huge diffs, fine for typical PRs.
        let rendered: Vec<Line<'static>> = lines
            .iter()
            .map(|l| render_diff_body_line(l, gutter_w, pad_to))
            .collect();
        let para = Paragraph::new(rendered)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(para, area);
    } else {
        // Non-wrap: pre-slice to viewport so we don't pay for
        // off-screen lines on huge diffs.
        let max = inner_height as usize;
        let visible: Vec<Line<'static>> = lines
            .iter()
            .skip(scroll)
            .take(max)
            .map(|l| render_diff_body_line(l, gutter_w, pad_to))
            .collect();
        frame.render_widget(
            Paragraph::new(visible).block(block).scroll((0, h_scroll)),
            area,
        );
    }
}

fn render_body_read(
    app: &App,
    frame: &mut Frame,
    area: Rect,
    file: &DiffFile,
    block: Block<'static>,
    inner_height: u16,
) {
    let diff = app.diff();
    let lines = match app.diff_read_cache_for(&file.path) {
        Some(l) => l,
        None => {
            let body = Paragraph::new(Line::from(Span::styled(
                "loading file…",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block);
            frame.render_widget(body, area);
            return;
        }
    };

    let max_lineno = lines.iter().map(|l| l.lineno).max().unwrap_or(0);
    let gutter_w = max_lineno.to_string().len().max(1);

    let pad_to = if diff.wrap {
        None
    } else {
        let pane = area.width.saturating_sub(4) as usize;
        let longest = lines
            .iter()
            .map(|l| read_line_visual_width(l, gutter_w))
            .max()
            .unwrap_or(0);
        Some(pane.max(longest))
    };

    let scroll = diff.read_scroll.get(&file.path).copied().unwrap_or(0);
    let h_scroll: u16 = app.diff_body_h_scroll().min(u16::MAX as usize) as u16;
    if diff.wrap {
        let rendered: Vec<Line<'static>> = lines
            .iter()
            .map(|l| render_read_body_line(l, gutter_w, pad_to))
            .collect();
        let para = Paragraph::new(rendered)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(para, area);
    } else {
        let max = inner_height as usize;
        let visible: Vec<Line<'static>> = lines
            .iter()
            .skip(scroll)
            .take(max)
            .map(|l| render_read_body_line(l, gutter_w, pad_to))
            .collect();
        frame.render_widget(
            Paragraph::new(visible).block(block).scroll((0, h_scroll)),
            area,
        );
    }
}

/// Visual width of a rendered diff-body row, used by the caller to
/// derive `pad_to`. Matches the column layout produced by
/// `render_diff_body_line`: `<old:gutter_w> <new:gutter_w> <sigil> <code>`.
/// Header / Hunk rows render the raw text without a gutter, so their
/// width is just the text width.
fn diff_line_visual_width(line: &DiffLine, gutter_w: usize) -> usize {
    use DiffLineKind;
    if line.kind == DiffLineKind::Header || line.kind == DiffLineKind::Hunk {
        return line.text.chars().count();
    }
    // `<old> <new> <sigil> ` → 2*gutter_w + 4 cells.
    let prefix = 2 * gutter_w + 4;
    let content = if line.spans.is_empty() {
        line.text.get(1..).unwrap_or("").chars().count()
    } else {
        line.spans.iter().map(|s| s.text.chars().count()).sum()
    };
    prefix + content
}

/// Visual width of a rendered read-body row. Matches
/// `render_read_body_line`'s `<lineno:gutter_w> <code>` layout (and
/// `<gutter_w> − N lines removed` for separators).
fn read_line_visual_width(line: &ReadLine, gutter_w: usize) -> usize {
    use ReadLineKind;
    let prefix = gutter_w + 1;
    let content = match line.kind {
        ReadLineKind::Separator { removed } => {
            if removed == 1 {
                "− 1 line removed".chars().count()
            } else {
                format!("− {removed} lines removed").chars().count()
            }
        }
        _ => {
            if line.spans.is_empty() {
                line.text.chars().count()
            } else {
                line.spans.iter().map(|s| s.text.chars().count()).sum()
            }
        }
    };
    prefix + content
}

/// Right-pad the spans with bg-tinted whitespace so the row's
/// background extends to `pad_to` cells. No-op when `pad_to` is
/// None (wrap mode) or when the content already exceeds the
/// target. `bg` is the colour to fill; for non-tinted rows we
/// still fill so the row's bg matches the terminal default —
/// callers pass `None` to skip the fill entirely.
fn fill_row_bg(spans: &mut Vec<Span<'static>>, pad_to: Option<usize>, bg: Option<Color>) {
    let Some(width) = pad_to else { return };
    let Some(bg) = bg else { return };
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used >= width {
        return;
    }
    let pad = " ".repeat(width - used);
    spans.push(Span::styled(pad, Style::default().bg(bg)));
}

/// Read-mode row tint. Added → green, Modified → blue, Separator
/// → red, Plain → none. Matches the diff body's green/red palette
/// so a side-by-side comparison reads consistently.
fn read_line_bg(kind: ReadLineKind) -> Option<Color> {
    use ReadLineKind;
    match kind {
        ReadLineKind::Added => Some(Color::Rgb(20, 38, 24)),
        ReadLineKind::Modified => Some(Color::Rgb(22, 30, 52)),
        ReadLineKind::Separator { .. } => Some(Color::Rgb(46, 22, 22)),
        ReadLineKind::Plain => None,
    }
}

/// Render one read-mode line. Plain → unstyled; Added/Modified →
/// row tint behind the gutter and source; Separator → synthetic
/// red row with a "− N lines removed" label centered in the gutter
/// + body area.
fn render_read_body_line(
    line: &ReadLine,
    gutter_w: usize,
    pad_to: Option<usize>,
) -> Line<'static> {
    use ReadLineKind;
    let bg = read_line_bg(line.kind);

    if let ReadLineKind::Separator { removed } = line.kind {
        // Synthetic row marking a deleted block. The gutter is
        // blank but takes the same width so adjacent lines stay
        // aligned. The body shows a faint count, left-aligned with
        // a single leading space so it doesn't collide with the
        // gutter divider.
        let label = if removed == 1 {
            "− 1 line removed".to_string()
        } else {
            format!("− {removed} lines removed")
        };
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(4);
        let gutter = format!("{:>width$} ", "", width = gutter_w);
        let red_dim = Color::Rgb(140, 90, 90);
        let row_bg = Style::default().bg(bg.unwrap_or(Color::Reset));
        spans.push(Span::styled(gutter, row_bg));
        spans.push(Span::styled(label, row_bg.fg(red_dim)));
        fill_row_bg(&mut spans, pad_to, bg);
        return Line::from(spans);
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 3);
    let mut gutter_style = Style::default().fg(Color::DarkGray);
    if let Some(c) = bg {
        gutter_style = gutter_style.bg(c);
    }
    let gutter = format!("{:>width$} ", line.lineno, width = gutter_w);
    spans.push(Span::styled(gutter, gutter_style));
    if line.spans.is_empty() {
        let mut style = Style::default();
        if let Some(c) = bg {
            style = style.bg(c);
        }
        spans.push(Span::styled(line.text.clone(), style));
    } else {
        for s in &line.spans {
            let mut style = s.style;
            if let Some(c) = bg {
                style = style.bg(c);
            }
            spans.push(Span::styled(s.text.clone(), style));
        }
    }
    fill_row_bg(&mut spans, pad_to, bg);
    Line::from(spans)
}

/// Render one diff line: `<old> <new> <sigil> <code>` with a bg
/// tint on Added / Removed / Hunk lines and syntect-derived spans
/// for the inner code.
fn render_diff_body_line(
    line: &DiffLine,
    gutter_w: usize,
    pad_to: Option<usize>,
) -> Line<'static> {
    use DiffLineKind;
    // Header lines (`diff --git`, `---`, `+++`, `index …`) keep
    // their full text and sit dim — no gutter, no sigil.
    if line.kind == DiffLineKind::Header {
        return Line::from(Span::styled(
            line.text.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    // Hunk headers fill the row width-ish with a cyan tint so
    // they stand out as section breaks.
    if line.kind == DiffLineKind::Hunk {
        return Line::from(Span::styled(
            line.text.clone(),
            Style::default()
                .fg(Color::Indexed(73))
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bg_tint = match line.kind {
        DiffLineKind::Added => Some(Color::Rgb(20, 38, 24)),
        DiffLineKind::Removed => Some(Color::Rgb(46, 22, 22)),
        _ => None,
    };
    let sigil = match line.kind {
        DiffLineKind::Added => "+",
        DiffLineKind::Removed => "−",
        DiffLineKind::Context => " ",
        _ => " ",
    };
    let sigil_color = match line.kind {
        DiffLineKind::Added => Color::Green,
        DiffLineKind::Removed => Color::Red,
        _ => Color::DarkGray,
    };

    fn fmt_no(no: Option<u32>, width: usize) -> String {
        match no {
            Some(n) => format!("{:>width$}", n, width = width),
            None => format!("{:>width$}", "", width = width),
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 5);
    let gutter = format!(
        "{} {} ",
        fmt_no(line.old_lineno, gutter_w),
        fmt_no(line.new_lineno, gutter_w),
    );
    let mut gutter_style = Style::default().fg(Color::DarkGray);
    if let Some(bg) = bg_tint {
        gutter_style = gutter_style.bg(bg);
    }
    spans.push(Span::styled(gutter, gutter_style));
    let mut sigil_style = Style::default()
        .fg(sigil_color)
        .add_modifier(Modifier::BOLD);
    if let Some(bg) = bg_tint {
        sigil_style = sigil_style.bg(bg);
    }
    spans.push(Span::styled(format!("{sigil} "), sigil_style));
    if line.spans.is_empty() {
        // No syntect spans — fall back to the raw inner text so
        // we still render something readable.
        let inner = line.text.get(1..).unwrap_or("");
        let mut style = Style::default();
        if let Some(bg) = bg_tint {
            style = style.bg(bg);
        }
        spans.push(Span::styled(inner.to_string(), style));
    } else {
        for s in &line.spans {
            let mut style = s.style;
            if let Some(bg) = bg_tint {
                style = style.bg(bg);
            }
            spans.push(Span::styled(s.text.clone(), style));
        }
    }
    fill_row_bg(&mut spans, pad_to, bg_tint);
    Line::from(spans)
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
    let groups = build_groups(app);
    if groups.is_empty() {
        let block = panel_block(" commands ", accent_of(app));
        let body = Paragraph::new(Line::from(Span::styled(
            "  (no items)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(body, area);
        return;
    }

    // Mark adjacent container + services groups so they render as one
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

    // Standalone groups use `items + 2` rows (top + bottom border).
    // JoinedTop omits its bottom border — the seam is owned by the
    // JoinedBottom group below it — so it only needs `items + 1`.
    // The last group absorbs the slack so we don't overflow.
    let last = groups.len() - 1;
    let constraints: Vec<Constraint> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let size = (g.len() as u16).saturating_add(joins[i].frame_rows());
            if i == last {
                Constraint::Min(size)
            } else {
                Constraint::Length(size)
            }
        })
        .collect();
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let global_idx = app.selected_index();
    // Refresh sidebar rect tracking. Each item maps 1:1 to a global
    // index in `app.items()` because `build_groups` preserves the
    // items() ordering (runtime → services → watchers → commands).
    let mut rects = app.sidebar_item_rects.borrow_mut();
    rects.clear();
    rects.resize(app.items().len(), Rect::default());
    let mut cursor = 0;
    for ((group, group_area), join) in groups.iter().zip(areas.iter()).zip(joins.iter()) {
        let local_selected = if global_idx >= cursor && global_idx < cursor + group.len() {
            Some(global_idx - cursor)
        } else {
            None
        };
        render_group(app, group, *group_area, local_selected, *join, frame);
        // Inner content rows of the block — both Top and Standalone
        // have a top border (1 row), and Bottom inherits a top border
        // too; the geometry of "first content row at area.y + 1" is
        // uniform across join positions.
        let inner_x = group_area.x.saturating_add(1);
        let inner_y = group_area.y.saturating_add(1);
        let inner_w = group_area.width.saturating_sub(2);
        let inner_h_raw = match join {
            JoinPosition::Standalone | JoinPosition::Bottom => group_area.height.saturating_sub(2),
            // Top half has no bottom border — its full content area
            // is height - 1 rows.
            JoinPosition::Top => group_area.height.saturating_sub(1),
        };
        for (local_i, _item) in group.items.iter().enumerate() {
            if (local_i as u16) >= inner_h_raw {
                // Group's panel is too short to show this item; leave
                // its rect as Default (zero-area → never hit-tested).
                continue;
            }
            rects[cursor + local_i] = Rect {
                x: inner_x,
                y: inner_y + local_i as u16,
                width: inner_w,
                height: 1,
            };
        }
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

/// Whether a sidebar group renders as its own bordered block or as
/// one half of a joined pair (container + services). The pair shares
/// a single horizontal seam — the top half drops its bottom border,
/// and the bottom half's top corners are redrawn as `├` / `┤` so the
/// two read as a single block divided by a divider line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JoinPosition {
    Standalone,
    Top,
    Bottom,
}

impl JoinPosition {
    fn borders(self) -> Borders {
        match self {
            JoinPosition::Standalone | JoinPosition::Bottom => Borders::ALL,
            JoinPosition::Top => Borders::TOP | Borders::LEFT | Borders::RIGHT,
        }
    }

    /// Number of non-content rows the block consumes. Top has no
    /// bottom border, so the seam belongs to the Bottom half.
    fn frame_rows(self) -> u16 {
        match self {
            JoinPosition::Top => 1,
            _ => 2,
        }
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
    let mut runtime = Vec::new();
    let mut services = Vec::new();
    let mut watchers = Vec::new();
    let mut commands = Vec::new();
    for item in app.items() {
        match item.kind {
            ItemKind::Runtime => runtime.push(item),
            ItemKind::Service => services.push(item),
            ItemKind::Watcher => watchers.push(item),
            ItemKind::Recipe | ItemKind::Script => commands.push(item),
        }
    }
    let mut out = Vec::new();
    if !runtime.is_empty() {
        out.push(SidebarGroup {
            label: "runtime",
            items: runtime,
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
    join: JoinPosition,
    frame: &mut Frame,
) {
    let accent = accent_of(app);
    let title_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            group.label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", group.items.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = panel_block_titled(title_line).borders(join.borders());

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
                .fg(SELECTION_FG)
                .bg(SELECTION_BG)
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

    // For the lower half of a joined pair, replace the rounded top
    // corners (╭ ╮) drawn by the block with tee connectors (├ ┤) so
    // the seam reads as a divider inside one block rather than the
    // top edge of a second block sitting below the first.
    if join == JoinPosition::Bottom && area.width >= 2 && area.height >= 1 {
        let style = Style::default().fg(Color::DarkGray);
        let buf = frame.buffer_mut();
        buf.set_string(area.x, area.y, "├", style);
        buf.set_string(area.x + area.width - 1, area.y, "┤", style);
    }
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
        ItemKind::Runtime | ItemKind::Service => "●",
        ItemKind::Watcher => "◇",
        // Same glyph for recipes and scripts — they share the
        // "commands" sidebar group. Kind is still distinguishable in
        // the detail pane and palette tags.
        ItemKind::Recipe | ItemKind::Script => "▸",
    }
}

fn item_indicator_style(app: &App, item: &Item) -> Style {
    match item.kind {
        ItemKind::Runtime => run_indicator_style(app.lifecycle_run()),
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
fn split_info_output(area: Rect, info_rows: usize) -> [Rect; 2] {
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
            kind_label(item.kind),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
        Span::raw(" "),
    ])
}

fn render_output_for_item(app: &App, item: &Item, accent: Color, frame: &mut Frame, area: Rect) {
    match item.kind {
        ItemKind::Runtime => match app.lifecycle_run() {
            Some(run) => render_run_buffer(run, accent, frame, area),
            None => render_idle_output(
                accent,
                frame,
                area,
                "no lifecycle action has run yet",
                Some("U up all · D down all · R restart all · S stop all"),
            ),
        },
        ItemKind::Service => {
            if let Some(service) = app.selected_service() {
                render_service_logs(service, accent, frame, area);
            }
        }
        ItemKind::Watcher => {
            if let Some(watcher) = app.selected_watcher() {
                render_watcher(watcher, accent, frame, area);
            }
        }
        ItemKind::Recipe | ItemKind::Script => match app.selected_run() {
            Some(run) => render_run_buffer(run, accent, frame, area),
            None => render_idle_output(accent, frame, area, "press enter to run", None),
        },
    }
}

/// Idle output panel — used by recipes / scripts / lifecycle row
/// when nothing has been run yet this session. Title carries an
/// "idle" badge instead of an exit code or duration.
fn render_idle_output(
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

fn render_run_buffer(run: &RunState, accent: Color, frame: &mut Frame, area: Rect) {
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

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Runtime => "runtime",
        ItemKind::Service => "service",
        ItemKind::Watcher => "watcher",
        ItemKind::Recipe => "recipe",
        ItemKind::Script => "script",
    }
}

fn kv(key: &str, value: &str) -> Line<'static> {
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

fn render_service_logs(service: &ServicePane, accent: Color, frame: &mut Frame, area: Rect) {
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

fn render_watcher(watcher: &WatcherPane, accent: Color, frame: &mut Frame, area: Rect) {
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
        WatcherState::Running => Style::default().fg(accent_of(app)),
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

fn render_palette(app: &App, palette: &Palette, accent: Color, frame: &mut Frame) {
    let outer = frame.area();
    let area = centered_rect(outer, 60, 16);
    frame.render_widget(Clear, area);

    let block = panel_block_titled(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "run …",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            palette.input().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(accent)),
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
                    .fg(SELECTION_FG)
                    .bg(SELECTION_BG)
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

    // Per-match rects so a click maps back to a `palette.matches()`
    // index. Matches outside the visible window get a zero rect;
    // `rect_contains` filters them out at hit-test time.
    let mut rects = palette.row_rects.borrow_mut();
    rects.clear();
    rects.resize(total, Rect::default());
    for (visible_offset, match_idx) in (start..end).enumerate() {
        let row = visible_offset as u16;
        if row >= layout[1].height {
            break;
        }
        rects[match_idx] = Rect {
            x: layout[1].x,
            y: layout[1].y + row,
            width: layout[1].width,
            height: 1,
        };
    }
}

fn render_worktree_switcher(
    switcher: &crate::tui::app::WorktreeSwitcher,
    accent: Color,
    frame: &mut Frame,
) {
    // Two sub-views: list (default) and the create form. They share
    // the same outer block; the form is taller because it has two
    // input rows plus a hint and an error line.
    if let Some(form) = switcher.creating.as_ref() {
        render_switcher_form(form, accent, frame);
    } else {
        render_switcher_list(switcher, accent, frame);
    }
}

fn render_switcher_list(
    switcher: &crate::tui::app::WorktreeSwitcher,
    accent: Color,
    frame: &mut Frame,
) {
    let total_rows = switcher.total_rows();
    // Body layout: list rows + blank + hint = total_rows + 2.
    // Block adds 2 (borders) + 2 (padding) = 4. Cap at 20.
    let height = (total_rows as u16 + 6).min(20);
    let area = centered_rect(frame.area(), 60, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "switch worktree",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Stack list above blank-row + hint. Stateful list paints the
    // full-row highlight; the old Paragraph + manually-styled spans
    // only colored the row text, leaving "+ new worktree" with a
    // visibly shorter highlight than the worktree-name rows above.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(total_rows as u16),
            Constraint::Length(1), // blank
            Constraint::Length(1), // hint
        ])
        .split(inner);

    let items: Vec<ListItem> = switcher
        .entries
        .iter()
        .map(|row| {
            let current_marker = if row.is_current {
                Span::styled(" ●", Style::default().fg(Color::Green))
            } else {
                Span::raw("  ")
            };
            let branch_label = row
                .branch
                .clone()
                .unwrap_or_else(|| "<detached>".to_string());
            ListItem::new(Line::from(vec![
                Span::raw(format!("{branch_label:<24}")),
                current_marker,
                Span::raw("  "),
                Span::styled(
                    row.path.display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .chain(std::iter::once(ListItem::new(Line::from(Span::styled(
            "+ new worktree",
            Style::default().fg(accent),
        )))))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(SELECTION_FG)
                .bg(SELECTION_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default();
    state.select(Some(switcher.selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    // Per-row rects keyed by switcher entry index (and the sentinel
    // at `entries.len()`). Rows beyond `chunks[0].height` get a zero
    // rect — the list height is sized to fit `total_rows`, so this is
    // really a fallback for the tightly-clamped Modal height.
    let offset = state.offset();
    let mut rects = switcher.row_rects.borrow_mut();
    rects.clear();
    rects.resize(total_rows, Rect::default());
    for i in 0..total_rows {
        if i < offset {
            continue;
        }
        let row = (i - offset) as u16;
        if row >= chunks[0].height {
            break;
        }
        rects[i] = Rect {
            x: chunks[0].x,
            y: chunks[0].y + row,
            width: chunks[0].width,
            height: 1,
        };
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ nav · enter switch · esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
}

fn render_switcher_form(form: &NewWorktreeForm, accent: Color, frame: &mut Frame) {
    use NewFormField;

    // Cap visible branch rows so a repo with hundreds of branches
    // doesn't blow up the modal — users narrow with the filter.
    const MAX_BRANCH_ROWS: usize = 8;
    let total_options = form.total_options();
    let list_rows = total_options.clamp(1, MAX_BRANCH_ROWS);
    // Body layout (inside the block, after vertical padding):
    //   1 row branch field + 1 row path field + 1 row blank +
    //   list_rows + 1 row blank + 1 row hint + 2 rows error?
    let body_rows = 2 + 1 + list_rows + 1 + 1 + if form.error.is_some() { 2 } else { 0 };
    // +4 = block borders (2) + vertical padding (2).
    let height = (body_rows as u16 + 4).min(24);
    let area = centered_rect(frame.area(), 64, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "new worktree",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let path_focus = matches!(form.focus, NewFormField::Path);
    let branch_focus = matches!(form.focus, NewFormField::Branch);

    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Slice the inner area into the layout we sized `height` for.
    // Stateful list in the middle gets full-width row highlights —
    // the previous Paragraph-based layout only colored the styled
    // span content, leaving the rest of the row uncolored.
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(1), // branch field
        Constraint::Length(1), // path field
        Constraint::Length(1), // blank
        Constraint::Length(list_rows as u16),
        Constraint::Length(1), // blank
        Constraint::Length(1), // hint
    ];
    if form.error.is_some() {
        constraints.push(Constraint::Length(1)); // blank
        constraints.push(Constraint::Length(1)); // error
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    frame.render_widget(
        Paragraph::new(field_row(
            "branch",
            &form.branch_input,
            branch_focus,
            accent,
        )),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(field_row("path", &form.path_input, path_focus, accent)),
        chunks[1],
    );

    if total_options == 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "(no branches)",
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[3],
        );
    } else {
        let items: Vec<ListItem> = (0..total_options)
            .map(|option_idx| ListItem::new(branch_row(form, option_idx, accent)))
            .collect();
        // Highlight only when the branch field is the focused one.
        // Path-focus mode still renders the list (so users see what
        // they'd be picking) but with no active row.
        let highlight_style = if branch_focus {
            Style::default()
                .fg(SELECTION_FG)
                .bg(SELECTION_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let list = List::new(items)
            .highlight_style(highlight_style)
            .highlight_spacing(HighlightSpacing::Always);
        let mut state = ListState::default();
        state.select(if branch_focus {
            Some(form.selected)
        } else {
            None
        });
        frame.render_stateful_widget(list, chunks[3], &mut state);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ pick · tab edit path · enter create · esc back",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[5],
    );
    if let Some(err) = form.error.as_ref() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(Color::Red),
            ))),
            chunks[7],
        );
    }
}

/// Render one row of the branch picker as a `Line`. The parent
/// `List` widget paints the full-width highlight when the row is
/// selected, so we don't need a per-row "▶ " marker. Existing
/// branches show as `<name>` (with a `[remote]` tag when remote-
/// only); the sentinel row reads `+ create branch '<input>' off HEAD`.
fn branch_row(
    form: &NewWorktreeForm,
    option_idx: usize,
    accent: Color,
) -> Line<'static> {
    let is_sentinel = option_idx == form.filtered.len() && form.show_create_sentinel();
    if is_sentinel {
        return Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("+ create branch '{}' off HEAD", form.branch_input),
                Style::default().fg(accent),
            ),
        ]);
    }
    let entry = &form.branches[form.filtered[option_idx]];
    let mut spans = vec![Span::raw(" "), Span::raw(entry.name.clone())];
    if entry.remote_only {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[remote]".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn field_row(label: &'static str, value: &str, focused: bool, accent: Color) -> Line<'static> {
    let label_style = if focused {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let cursor_span = if focused {
        Span::styled("█", Style::default().fg(accent))
    } else {
        Span::raw("")
    };
    Line::from(vec![
        Span::styled(format!("{label:<8}"), label_style),
        Span::styled(value.to_string(), value_style),
        cursor_span,
    ])
}

fn render_args_prompt(prompt: &ArgsPrompt, accent: Color, frame: &mut Frame) {
    let area = centered_rect(frame.area(), 60, 7);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("args for `{}`", prompt.item_name),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let body = vec![
        Line::from(vec![
            Span::styled("> ", Style::default().fg(accent)),
            Span::styled(
                prompt.input.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            // Block-character cursor at the end so users see where input goes.
            Span::styled("█", Style::default().fg(accent)),
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

fn render_confirm_modal(
    app: &App,
    dialog: &ConfirmDialog,
    accent: Color,
    frame: &mut Frame,
) {
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
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let yes_style = if dialog.yes_focused {
        Style::default()
            .fg(SELECTION_FG)
            .bg(SELECTION_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let no_style = if !dialog.yes_focused {
        Style::default()
            .fg(SELECTION_FG)
            .bg(SELECTION_BG)
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

    // Compute button rects from the block's content area before the
    // block is moved into Paragraph. Button labels are 5 / 4 cols
    // (" Yes " / " No "), separated by 4 spaces of padding.
    let content = block.inner(area);
    let buttons_y = content.y + 2;
    let yes_w = 5u16;
    let no_w = 4u16;
    let sep = 4u16;
    app.confirm_yes_rect.set(Some(Rect {
        x: content.x,
        y: buttons_y,
        width: yes_w,
        height: 1,
    }));
    app.confirm_no_rect.set(Some(Rect {
        x: content.x + yes_w + sep,
        y: buttons_y,
        width: no_w,
        height: 1,
    }));

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
        crate::tui::app::View::ControlCenter => control_center_hints(app),
        crate::tui::app::View::Terminals => vec![
            ("↑↓", "nav"),
            ("enter", "attach"),
            ("n", "new"),
            ("d", "delete"),
            ("C", "control"),
            ("G", "diff"),
            ("W", "worktree"),
            ("q", "quit"),
        ],
        crate::tui::app::View::Diff => diff_hints(app),
    }
}

fn diff_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let in_read = app.diff_body_mode() == BodyMode::Read;
    let mut hints: Vec<(&'static str, &'static str)> = match app.diff_focus() {
        DiffFocus::Files => {
            let mut h: Vec<(&'static str, &'static str)> = vec![("↑↓", "file"), ("tab", "body")];
            if !in_read {
                h.push(("]/[", "hunk"));
            }
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
    hints.push(match app.diff_body_mode() {
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
    }
    hints.push(("/", "palette"));
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

fn panel_block(title: &'static str, accent: Color) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
        app.switcher_select_next();
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
        app.diff_set_files(vec![
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
        app.set_tmux_available(true);
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
        app.set_tmux_available(true);
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
        app.set_tmux_available(true);
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
