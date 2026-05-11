//! Single source of truth for diff/read body line widths in cells.
//!
//! Both the renderer (`render_diff_body_line` / `render_read_body_line`)
//! and the horizontal-scroll clamp consume these — keeping them in
//! one file eliminates the "kept in sync by hand" smell the earlier
//! split carried.
//!
//! Layout reference:
//!   - **Diff body content row**: `<old_lineno:gutter_w> <new_lineno:gutter_w> <sigil><space><content>`
//!     = `2 * gutter_w + 4 + content_chars`.
//!   - **Diff body header/hunk row**: raw text, no gutter.
//!   - **Read body row**: `<lineno:gutter_w><space><content>` = `gutter_w + 1 + content_chars`.
//!   - **Read body separator row**: `<gutter_w blanks> − N lines removed` = `gutter_w + 1 + label_chars`.

use crate::tui::views::diff::state::{DiffLine, DiffLineKind, ReadLine, ReadLineKind};

/// Rendered width of one diff body line, in cells.
pub fn diff_line_rendered_width(line: &DiffLine, gutter_w: usize) -> usize {
    match line.kind {
        DiffLineKind::Header | DiffLineKind::Hunk => line.text.chars().count(),
        DiffLineKind::Added | DiffLineKind::Removed | DiffLineKind::Context => {
            let content_chars = if line.spans.is_empty() {
                // Renderer falls back to `line.text.get(1..)` (strips
                // the leading sigil from the raw line) when no spans
                // are present.
                line.text.chars().count().saturating_sub(1)
            } else {
                line.spans.iter().map(|s| s.text.chars().count()).sum()
            };
            2 * gutter_w + 4 + content_chars
        }
    }
}

/// Rendered width of one read-mode body line, in cells.
pub fn read_line_rendered_width(line: &ReadLine, gutter_w: usize) -> usize {
    match line.kind {
        ReadLineKind::Separator { removed } => {
            let label_chars = if removed == 1 {
                "− 1 line removed".chars().count()
            } else {
                format!("− {removed} lines removed").chars().count()
            };
            gutter_w + 1 + label_chars
        }
        _ => {
            let content_chars = if line.spans.is_empty() {
                line.text.chars().count()
            } else {
                line.spans.iter().map(|s| s.text.chars().count()).sum()
            };
            gutter_w + 1 + content_chars
        }
    }
}
