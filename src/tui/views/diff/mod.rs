//! Diff view: file-level git review (`g`).
//!
//! Phase 1 has the leaf diff/read line/file types; Phase 3 moves the
//! full `DiffView` (state, render, input, git shell-outs) here, and
//! consolidates the duplicated line-width formulas into
//! `line_width.rs`.

pub mod state;
