//! scaffl TUI — embedded dashboard.
//!
//! Phase 3 work. This stub exists so the workspace builds end-to-end and the
//! CLI can compile against the future entry point without churn.

#![allow(clippy::module_inception)]

/// Placeholder. Will be replaced by the `App` type that owns the ratatui
/// runtime, pane registry, and palette state.
pub struct TuiApp;

impl TuiApp {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new()
    }
}
