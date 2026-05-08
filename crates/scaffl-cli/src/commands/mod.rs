//! Built-in subcommand implementations.
//!
//! One module per built-in to keep `app.rs` focused on dispatch and
//! help-text wiring. These commands deliberately avoid spawning a
//! [`scaffl_runtime::Executor`] when they don't need one — `env` reuses
//! [`scaffl_runtime::Env::resolve`] directly, `doctor` uses pure-function
//! validation, and `init` is filesystem-only.

pub mod doctor;
pub mod env;
pub mod hooks;
pub mod init;
