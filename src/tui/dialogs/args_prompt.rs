//! Args-prompt dialog state.
//!
//! Opened when the user activates a row whose runnable declares
//! `forward_args = true`. Selection is locked while the prompt is up.

use crate::tui::views::control_center::state::ItemKind;

/// Open args prompt for a `forward_args = true` row. Only one is
/// open at a time; selection is locked while the prompt is up.
#[derive(Debug, Clone)]
pub struct ArgsPrompt {
    pub item_name: String,
    pub kind: ItemKind,
    pub input: String,
}
