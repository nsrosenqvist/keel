//! Render for the command palette overlay.

use crate::tui::app::App;
use crate::tui::palette::Palette;
use crate::tui::ui::{SELECTION_BG, SELECTION_FG, centered_rect, panel_block_titled, window};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

pub fn render(app: &App, palette: &Palette, accent: Color, frame: &mut Frame) {
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
            let kind = item.kind.label();
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
