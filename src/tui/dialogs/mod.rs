//! Modal dialogs.
//!
//! Phase 1 has only the leaf state types per dialog; Phase 4 fuses
//! the four `Option<...>` modal slots on `App` (plus the redundant
//! `Mode` enum) into a single `Modal` enum and gives each dialog its
//! full triplet (state + render + input).

pub mod args_prompt;
pub mod confirm;
pub mod switcher;
