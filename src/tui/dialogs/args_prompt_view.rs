//! Render for the args-prompt modal.

use crate::tui::dialogs::args_prompt::ArgsPrompt;
use crate::tui::ui::centered_rect;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

pub fn render(prompt: &ArgsPrompt, accent: Color, frame: &mut Frame) {
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
