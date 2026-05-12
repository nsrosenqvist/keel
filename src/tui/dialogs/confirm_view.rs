//! Render for the yes/no confirmation modal.

use crate::tui::app::App;
use crate::tui::dialogs::confirm::ConfirmDialog;
use crate::tui::ui::{SELECTION_BG, SELECTION_FG, centered_rect};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

pub fn render(app: &App, dialog: &ConfirmDialog, accent: Color, frame: &mut Frame) {
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
