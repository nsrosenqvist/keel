//! Shared grouped-sidebar layout.
//!
//! Both the control-center sidebar and the terminals sidebar stack
//! a small number of bordered group blocks vertically, highlight a
//! single selected row, and populate a per-row rect buffer so the
//! mouse handler can hit-test wheel/click events. Before this lived
//! here each renderer reimplemented the constraint math, the
//! border-omission logic for joined groups, the selection-state
//! `ListState`, and the rect bookkeeping — ~60% overlap kept in
//! sync by hand.
//!
//! Callers build a `Vec<SidebarGroup>` (one per visible section)
//! with pre-rendered `ListItem`s and pass it here along with the
//! per-group [`JoinPosition`]s and a mutable rect buffer. The
//! caller owns item rendering (glyphs, badges, kind-specific
//! columns) — this module owns the geometry + chrome.

use crate::tui::ui::{SELECTION_BG, SELECTION_FG, panel_block_titled};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, HighlightSpacing, List, ListItem, ListState};

/// One bordered group block in a grouped sidebar.
pub struct SidebarGroup<'a> {
    /// Label shown in the block's title; rendered with the active
    /// view accent + bold and a dim `(<count>)` suffix.
    pub label: &'static str,
    /// Pre-rendered rows the caller wants in this group, in order.
    pub items: Vec<ListItem<'a>>,
    /// Local row index (within `items`) of the selected row, or
    /// `None` when the selection is in a different group.
    pub selected_local: Option<usize>,
}

impl<'a> SidebarGroup<'a> {
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Whether a sidebar group renders as its own bordered block or as
/// one half of a joined pair. The pair shares a single horizontal
/// seam — the top half drops its bottom border, and the bottom
/// half's top corners are redrawn as `├` / `┤` so the two read as
/// a single block divided by a divider line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JoinPosition {
    Standalone,
    Top,
    Bottom,
}

impl JoinPosition {
    pub fn borders(self) -> Borders {
        match self {
            JoinPosition::Standalone | JoinPosition::Bottom => Borders::ALL,
            JoinPosition::Top => Borders::TOP | Borders::LEFT | Borders::RIGHT,
        }
    }

    /// Number of non-content rows the block consumes. Top has no
    /// bottom border, so the seam belongs to the Bottom half.
    pub fn frame_rows(self) -> u16 {
        match self {
            JoinPosition::Top => 1,
            _ => 2,
        }
    }
}

/// Render a stack of grouped lists with optional join seam between
/// consecutive groups. The `rects` buffer is overwritten with one
/// rect per row in flat order across all groups (matches the
/// caller's index space, so a global index → rect lookup is a
/// straight Vec subscript). Off-screen rows get `Rect::default()`
/// so a click can never match a row the layout couldn't fit.
///
/// `highlight_symbol` is the gutter glyph drawn next to the
/// selected row (`Some("▶ ")` in the control center, `None` in the
/// terminals view). `accent` colours the per-group titles in bold.
///
/// For any `JoinPosition::Bottom` group, the rounded top corners
/// (`╭ ╮`) drawn by the block are replaced with tee connectors
/// (`├ ┤`) so the joined pair reads as one block divided by a
/// divider line.
pub fn render_grouped_sidebar(
    frame: &mut Frame,
    area: Rect,
    groups: &[SidebarGroup<'_>],
    joins: &[JoinPosition],
    accent: Color,
    rects: &mut Vec<Rect>,
    highlight_symbol: Option<&'static str>,
) {
    debug_assert_eq!(groups.len(), joins.len());
    let total_rows: usize = groups.iter().map(|g| g.len()).sum();
    rects.clear();
    rects.resize(total_rows, Rect::default());

    if groups.is_empty() {
        return;
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

    let mut cursor = 0;
    for ((group, group_area), join) in groups.iter().zip(areas.iter()).zip(joins.iter()) {
        render_group(frame, group, *group_area, *join, accent, highlight_symbol);
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
        for local_i in 0..group.len() {
            if (local_i as u16) >= inner_h_raw {
                // Group's panel is too short to show this row; leave
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

fn render_group(
    frame: &mut Frame,
    group: &SidebarGroup<'_>,
    area: Rect,
    join: JoinPosition,
    accent: Color,
    highlight_symbol: Option<&'static str>,
) {
    let title_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            group.label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}) ", group.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = panel_block_titled(title_line).borders(join.borders());

    let mut list = List::new(group.items.clone())
        .block(block)
        .highlight_style(
            Style::default()
                .fg(SELECTION_FG)
                .bg(SELECTION_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    if let Some(sym) = highlight_symbol {
        list = list.highlight_symbol(sym);
    }
    let mut state = ListState::default();
    state.select(group.selected_local);
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
