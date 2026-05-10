//! `scaffl install` — first-time project setup, idempotent on re-run.
//!
//! Submodules:
//!
//! - [`plan`] resolves the `[install]` config and `.scaffl/install/*`
//!   discovery into an ordered, executable list of steps.
//! - [`state`] reads and atomically writes `.scaffl/install.state.json`.
//! - [`renderer`] is a crossterm line-redraw printer that updates the
//!   step list in place and hands the terminal to interactive steps.
//! - [`runner`] drives the plan: spawns each step, streams output into
//!   the renderer, and decides ok / skipped / failed per step.
//!
//! This module exposes [`run`] as the single entry point; `app.rs`
//! dispatches `Command::Install` to it.

pub mod plan;
pub mod renderer;
pub mod runner;
pub mod state;

pub use runner::{InstallArgs, run};
