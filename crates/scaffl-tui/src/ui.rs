//! Rendering — pure function from [`App`] state to a ratatui [`Frame`].

use crate::app::{App, ItemKind, Mode};
use crate::palette::Palette;
use crate::runner::CapturedLine;
use crate::services::ServicePane;
use crate::watchers::{WatcherPane, WatcherState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use scaffl_config::Run;
use scaffl_container::ServiceStatus;
use scaffl_runtime::OutputStream;

const SIDEBAR_RATIO: u16 = 25;
const STATUS_BAR_HEIGHT: u16 = 1;
const HOTKEY_HINTS: &str = "↑↓ navigate · enter run · : palette · q quit";

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

    // Right pane:
    //   - if a service is selected, show its tailed logs (full height)
    //   - if a run is in progress / finished, split detail + output
    //   - otherwise just detail
    if let Some(service) = app.selected_service() {
        render_service_logs(service, frame, body[1]);
    } else if let Some(watcher) = app.selected_watcher() {
        render_watcher(watcher, frame, body[1]);
    } else if app.current_run().is_some() {
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(body[1]);
        render_detail(app, frame, right[0]);
        render_output(app, frame, right[1]);
    } else {
        render_detail(app, frame, body[1]);
    }

    render_status(app, frame, outer[1]);

    if app.mode() == Mode::Palette {
        if let Some(palette) = app.palette() {
            render_palette(app, palette, frame);
        }
    }
}

fn render_palette(app: &App, palette: &Palette, frame: &mut Frame) {
    let outer = frame.area();
    let area = centered_rect(outer, 60, 16);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" run … (Esc cancels) ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    // Input line.
    let input_line = Line::from(vec![
        Span::styled("❯ ", Style::default().fg(Color::Cyan)),
        Span::styled(
            palette.input().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(input_line), layout[0]);

    // Match list — show enough to fill the area, with the selected item
    // marked. Names render with their kind tag so users know what they
    // launch.
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
            let kind = match item.kind {
                ItemKind::Recipe => "recipe",
                ItemKind::Script => "script",
                ItemKind::Service => "service",
                ItemKind::Watcher => "watcher",
            };
            let style = if idx == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!("  {:<24} ", item.name),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("[{kind}]"), style.fg(Color::DarkGray)),
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
            Constraint::Length((area.height.saturating_sub(h)) / 2),
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

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = app
        .items()
        .iter()
        .map(|item| {
            let glyph = match item.kind {
                ItemKind::Service => "●",
                ItemKind::Watcher => "◇",
                ItemKind::Recipe => "▸",
                ItemKind::Script => "▪",
            };
            let glyph_style = match item.kind {
                ItemKind::Service => service_indicator_style(app, &item.name),
                ItemKind::Watcher => watcher_indicator_style(app, &item.name),
                _ => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{glyph} "), glyph_style),
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
                ItemKind::Service => "service",
                ItemKind::Watcher => "watcher",
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
    } else if app.selected_watcher().is_some() {
        // Watcher detail is rendered by render_watcher in the right-pane
        // chooser. This branch is unreachable in practice.
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

fn render_output(app: &App, frame: &mut Frame, area: Rect) {
    let Some(run) = app.current_run() else {
        return;
    };

    let title = format!(" {} ", run.status_label());
    let block = Block::default().title(title).borders(Borders::ALL);

    // Show the most recent lines that fit in the pane (height − borders).
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

fn render_captured_line(line: &CapturedLine) -> Line<'static> {
    let style = match line.stream {
        OutputStream::Stdout => Style::default(),
        OutputStream::Stderr => Style::default().fg(Color::Red),
    };
    Line::from(Span::styled(line.text.clone(), style))
}

fn render_service_logs(service: &ServicePane, frame: &mut Frame, area: Rect) {
    let title = match (&service.tail_error, service.status) {
        (Some(err), _) => format!(" {} · ! {} ", service.name, truncate(err, 60)),
        (None, Some(s)) => format!(" {} · {} ", service.name, status_word(s)),
        (None, None) => format!(" {} · ? ", service.name),
    };
    let block = Block::default().title(title).borders(Borders::ALL);

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
        vec![Line::from(Span::styled(
            "(waiting for output...)".to_string(),
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
    };

    let paragraph = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_watcher(watcher: &WatcherPane, frame: &mut Frame, area: Rect) {
    let title = format!(" {} ", watcher.status_label());
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(1)])
        .split(inner);

    // Header: recipe + globs + debounce.
    let mut header_lines: Vec<Line<'static>> = Vec::new();
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

    // Output buffer.
    let max_lines = layout[1].height.saturating_sub(0) as usize;
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
                "(no run yet — edit a watched file)".to_string()
            }
            WatcherState::Idle => "(idle — buffer cleared on next run)".into(),
            WatcherState::Debouncing => "(cooldown...)".into(),
            WatcherState::Running => "(starting...)".into(),
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
        WatcherState::Running => Style::default().fg(Color::Cyan),
    }
}

fn service_indicator_style(app: &App, service: &str) -> Style {
    match app.services().get(service).and_then(|p| p.status) {
        Some(ServiceStatus::Running) => Style::default().fg(Color::Green),
        Some(ServiceStatus::Stopped) => Style::default().fg(Color::Yellow),
        Some(ServiceStatus::Missing) => Style::default().fg(Color::Red),
        None => Style::default().fg(Color::DarkGray),
    }
}

fn status_word(s: ServiceStatus) -> &'static str {
    match s {
        ServiceStatus::Running => "running",
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Missing => "missing",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(flash) = &app.flash {
        let p = Paragraph::new(Line::from(Span::styled(
            flash.clone(),
            Style::default().fg(Color::Yellow),
        )));
        frame.render_widget(p, area);
        return;
    }
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
