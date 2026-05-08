//! Rendering — pure function from [`App`] state to a ratatui [`Frame`].

use crate::app::{App, ItemKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use scaffl_config::Run;

const SIDEBAR_RATIO: u16 = 30;
const STATUS_BAR_HEIGHT: u16 = 1;
const HOTKEY_HINTS: &str = "↑↓ navigate · g/G first/last · q quit · enter run (TODO)";

pub fn render(app: &App, frame: &mut Frame) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(STATUS_BAR_HEIGHT)])
        .split(frame.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(SIDEBAR_RATIO),
            Constraint::Percentage(100 - SIDEBAR_RATIO),
        ])
        .split(outer[0]);

    render_sidebar(app, frame, body[0]);
    render_detail(app, frame, body[1]);
    render_status(app, frame, outer[1]);
}

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = app
        .items()
        .iter()
        .map(|item| {
            let glyph = match item.kind {
                ItemKind::Recipe => "▸",
                ItemKind::Script => "▪",
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(Color::DarkGray)),
                Span::raw(item.name.clone()),
            ]))
        })
        .collect();

    let title = format!(" commands ({}) ", app.items().len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Black)
                .bg(Color::Cyan),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    if !app.items().is_empty() {
        state.select(Some(app.selected_index()));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default().title(" details ").borders(Borders::ALL);
    let lines = build_detail_lines(app);
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn build_detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(item) = app.selected_item() else {
        return vec![Line::from(Span::raw("No commands defined."))];
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            match item.kind {
                ItemKind::Recipe => "recipe",
                ItemKind::Script => "script",
            },
            Style::default().fg(Color::DarkGray),
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

fn render_run(run: &Run) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "run",
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    match run {
        Run::Single(s) => out.push(Line::from(Span::raw(format!("  {s}")))),
        Run::Steps(steps) => {
            for s in steps {
                out.push(Line::from(Span::raw(format!("  • {s}"))));
            }
        }
    }
    out
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn render_status(_app: &App, frame: &mut Frame, area: Rect) {
    let p = Paragraph::new(Line::from(vec![Span::styled(
        HOTKEY_HINTS,
        Style::default().fg(Color::DarkGray),
    )]));
    frame.render_widget(p, area);
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
                [command.up]
                desc = "Start"
                run = "docker compose up"

                [command.test]
                run = "composer test"
                in = "app"
                forward_args = true
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
}
