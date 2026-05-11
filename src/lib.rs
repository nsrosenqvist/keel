//! keel — a dev-loop wrapper that adapts to your project.
//!
//! Each top-level module is a bounded context. Concrete responsibilities:
//!
//! - [`config`] — parse and validate user-authored configuration into
//!   immutable value objects.
//! - [`cache`] — content-addressed git cache shared by `hooks` and `agents`.
//! - [`container`] — abstraction over container backends (compose, custom,
//!   devcontainer, null).
//! - [`runtime`] — recipe resolution and execution; orchestrates env,
//!   processes, services, and backends.
//! - [`hooks`] — git hook installation and pre-commit-config compatibility.
//! - [`agents`] — manage agent instructions sourced from upstream repos.
//! - [`tui`] — embedded TUI dashboard.
//! - [`cli`] — argv parsing and dispatch; the binary entry lives in
//!   `src/main.rs`.

pub mod agents;
pub mod cache;
pub mod cli;
pub mod config;
pub mod container;
pub mod hooks;
pub mod runtime;
pub mod tui;
