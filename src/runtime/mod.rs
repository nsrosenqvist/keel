//! keel runtime — recipe resolution and execution.
//!
//! Bounded context: turning a parsed [`crate::config::Config`] plus a CLI
//! invocation into actual process work, while talking to a
//! [`crate::container::Backend`] for anything container-shaped. The CLI and
//! the TUI both consume this crate; they do not duplicate any of its logic.

pub mod env;
pub mod error;
pub mod executor;
pub mod ports;
pub mod resolver;
pub mod services;
pub mod sink;
pub mod worktree;

pub use env::Env;
pub use error::RuntimeError;
pub use executor::{Executor, WorkspaceTarget};
pub use ports::{RecipeProvider, ScriptProvider};
pub use resolver::{Resolution, Resolver, ResolverContext};
pub use sink::{ChannelSink, InheritSink, OutputLine, OutputSink, OutputStream};
pub use worktree::{
    BaseRef, BranchEntry, Identity, WorktreeListEntry, detect_trunk, git_toplevel, list_branches,
    merge_base, slugify,
};
