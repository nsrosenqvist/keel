//! Render for the worktree-switcher modal — list view + create form.

use crate::tui::dialogs::switcher::{NewFormField, NewWorktreeForm, WorktreeSwitcher};
use crate::tui::ui::{SELECTION_BG, SELECTION_FG, centered_rect};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Padding,
    Paragraph,
};

/// Top-level switcher render: dispatches to the list view or the
/// create-worktree form depending on `switcher.creating`.
pub fn render(switcher: &WorktreeSwitcher, accent: Color, frame: &mut Frame) {
    // Two sub-views: list (default) and the create form. They share
    // the same outer block; the form is taller because it has two
    // input rows plus a hint and an error line.
    if let Some(form) = switcher.creating.as_ref() {
        render_form(form, accent, frame);
    } else {
        render_list(switcher, accent, frame);
    }
}

fn render_list(switcher: &WorktreeSwitcher, accent: Color, frame: &mut Frame) {
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

fn render_form(form: &NewWorktreeForm, accent: Color, frame: &mut Frame) {
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
fn branch_row(form: &NewWorktreeForm, option_idx: usize, accent: Color) -> Line<'static> {
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
