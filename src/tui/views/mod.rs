//! View modules. One bounded context per visible TUI surface.
//!
//! Each view owns its state, render, and input dispatch. Phase 1 of the
//! TUI refactor moves the pure leaf types here; later phases promote
//! the impl blocks and dispatch.

pub mod control_center;
pub mod diff;
pub mod terminals;
