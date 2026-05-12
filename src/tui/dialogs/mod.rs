//! Modal dialogs.
//!
//! The TUI's four modal surfaces — palette, confirm dialog,
//! args-prompt, and worktree switcher — used to live as four
//! parallel `Option<...>` slots on `App`, plus a redundant
//! `Mode` enum that mirrored which one was `Some`. Phase 4 fuses
//! them into a single [`Modal`] enum so a single `Option<Modal>`
//! field carries both "which modal is open" and "what state does
//! it hold."

use crate::tui::dialogs::args_prompt::ArgsPrompt;
use crate::tui::dialogs::confirm::ConfirmDialog;
use crate::tui::dialogs::switcher::WorktreeSwitcher;
use crate::tui::palette::Palette;

pub mod args_prompt;
pub mod args_prompt_view;
pub mod confirm;
pub mod confirm_view;
pub mod palette_view;
pub mod switcher;
pub mod switcher_view;

/// Which modal surface is currently open, plus its state. The
/// `App` carries this as `Option<Modal>`; `None` is "normal mode"
/// (keys go to the active view's handler).
pub enum Modal {
    Palette(Palette),
    Confirm(ConfirmDialog),
    ArgsPrompt(ArgsPrompt),
    Switcher(WorktreeSwitcher),
}

/// Discriminant tag matching [`Modal`] variants — what the old
/// `Mode` enum tracked when the four `Option<...>` fields and
/// `Mode` were kept in lockstep. Kept around for ergonomics in
/// key-dispatch matches; populated by [`Modal::tag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalTag {
    Palette,
    Confirm,
    ArgsPrompt,
    Switcher,
}

impl Modal {
    pub fn tag(&self) -> ModalTag {
        match self {
            Modal::Palette(_) => ModalTag::Palette,
            Modal::Confirm(_) => ModalTag::Confirm,
            Modal::ArgsPrompt(_) => ModalTag::ArgsPrompt,
            Modal::Switcher(_) => ModalTag::Switcher,
        }
    }
}
