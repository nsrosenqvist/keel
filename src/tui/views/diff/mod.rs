//! Diff view: file-level git review (`g`).
//!
//! Phase 3 of the TUI refactor consolidated the duplicated body-line
//! width formulas into [`line_width`]; the wider state / render /
//! input split lands in subsequent commits inside the same phase.

pub mod line_width;
pub mod state;
