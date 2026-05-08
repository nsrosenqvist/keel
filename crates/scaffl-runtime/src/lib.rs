//! scaffl runtime — recipe resolution and execution.
//!
//! Bounded context: turning a parsed [`scaffl_config::Config`] plus a CLI
//! invocation into actual process work, while talking to a
//! [`scaffl_container::Backend`] for anything container-shaped. The CLI and
//! the TUI both consume this crate; they do not duplicate any of its logic.

pub mod env;
pub mod error;
pub mod executor;
pub mod resolver;

pub use env::Env;
pub use error::RuntimeError;
pub use executor::Executor;
pub use resolver::{Resolution, Resolver, ResolverContext};
