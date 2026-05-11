//! Devcontainer integration: parser, container identity, and the
//! `Backend` impl that drives `docker` directly.
//!
//! See `docs/devcontainer.md` for the user-facing supported subset.
//! The bounded context here is "everything that talks to the docker
//! CLI on behalf of a devcontainer"; the executor and TUI consume the
//! API exposed by [`DevcontainerBackend`].

pub mod backend;
pub mod config;

pub use backend::{DevcontainerBackend, DevcontainerError, DevcontainerIdentity, EnsurePlan};
pub use config::{ContainerSource, DevcontainerConfigError, DevcontainerSpec};
