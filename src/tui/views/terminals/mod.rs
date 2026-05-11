//! Terminals view: tmux-backed shells, one session per worktree.
//!
//! State, render, input, and tmux shell-outs live together under
//! their own module so the rest of the TUI only sees the view's
//! public surface.

pub mod input;
pub mod state;
pub mod tmux;
pub mod view;
