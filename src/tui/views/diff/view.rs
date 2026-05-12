//! Render for the Diff view.
//!
//! Entry point: [`render`]. Three stacked panels share the area:
//! a top-left comparison banner, the file list below it, and the
//! diff/read body on the right. Body rendering branches on
//! [`crate::tui::views::diff::state::BodyMode`] into either the
//! unified-diff path or the read-full-file path; both share the
//! line-rendering helpers (`render_diff_body_line`,
//! `render_read_body_line`) and the bg-padding helper
//! (`fill_row_bg`).

use crate::tui::app::App;
use crate::tui::shared::scroll::Axis;
use crate::tui::ui::{SELECTION_BG, SELECTION_FG, SIDEBAR_RATIO, accent_of, panel_block_titled};
use crate::tui::views::diff::line_width::{diff_line_rendered_width, read_line_rendered_width};
use crate::tui::views::diff::state::{
    BodyMode, DiffFile, DiffFocus, DiffLine, DiffLineKind, DiffStatus, ReadLine, ReadLineKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
    Wrap,
};

/// Real Diff body: file list sidebar + diff body right pane.
pub fn render(app: &App, frame: &mut Frame, area: Rect) {
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
    render_header(app, frame, left[0]);
    // Stash the file-list and body rects so the mouse handler can
    // hit-test wheel/click events. Updated every frame so a window
    // resize is reflected on the next click.
    app.diff().files_rect.set(Some(left[1]));
    app.diff().body_rect.set(Some(body[1]));
    render_files(app, frame, left[1]);
    render_body(app, frame, body[1]);
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
fn render_header(app: &App, frame: &mut Frame, area: Rect) {
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

fn render_files(app: &App, frame: &mut Frame, area: Rect) {
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

fn render_body(app: &App, frame: &mut Frame, area: Rect) {
    let accent = accent_of(app);
    let diff = app.diff();
    let focused = diff.focus == DiffFocus::Body;
    let mode = diff.body_mode;
    let mode_label = match mode {
        BodyMode::Diff => "diff",
        BodyMode::Read => "read",
    };
    let title_text = match app.diff().selected_file() {
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

    let Some(file) = app.diff().selected_file() else {
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
    let lines = match app.diff().cache_for(&file.path) {
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
            .map(|l| diff_line_rendered_width(l, gutter_w))
            .max()
            .unwrap_or(0);
        Some(pane.max(longest))
    };

    let scroll = diff.diff_scroll.get(&file.path, Axis::Vertical);
    // `body_h_scroll` clamps to 0 when wrap is on — wrap mode
    // has no horizontal axis, and a stale map entry must not bleed
    // into rendering. Cast saturates so a >u16::MAX offset (which
    // would be a bug elsewhere) doesn't wrap around.
    let h_scroll: u16 = app.diff().body_h_scroll().min(u16::MAX as usize) as u16;
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
    let lines = match app.diff().read_cache_for(&file.path) {
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
            .map(|l| read_line_rendered_width(l, gutter_w))
            .max()
            .unwrap_or(0);
        Some(pane.max(longest))
    };

    let scroll = diff.read_scroll.get(&file.path, Axis::Vertical);
    let h_scroll: u16 = app.diff().body_h_scroll().min(u16::MAX as usize) as u16;
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
fn render_read_body_line(line: &ReadLine, gutter_w: usize, pad_to: Option<usize>) -> Line<'static> {
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
fn render_diff_body_line(line: &DiffLine, gutter_w: usize, pad_to: Option<usize>) -> Line<'static> {
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
