//! Terminals view: tmux-backed shells, one session per worktree.
//!
//! Phase 1 has only the row + window leaf types; Phase 2 moves the
//! full `TerminalsView` (state + render + input + tmux shell-outs) here.

pub mod state;
