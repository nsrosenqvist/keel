//! Manage agent instructions and skills (CLAUDE.md, AGENTS.md,
//! `.claude/skills/`, …) sourced from one or more upstream git
//! repos.
//!
//! Three concerns kept separable:
//!
//! - [`manifest`] parses the upstream `keel-agents.toml` that
//!   declares which files map to which destinations.
//! - [`merge`] applies downstream overrides + expands `[[dir]]`
//!   mappings against the cloned worktree.
//! - [`mod@apply`] orchestrates cache (via `ampelos::cache`) → manifest →
//!   merge → drift / collision / shadow checks → file writes,
//!   maintaining `.keel/agents.state.json` along the way.

pub mod apply;
pub mod error;
pub mod manifest;
pub mod merge;
pub mod state;

pub use apply::{
    ApplyOptions, ApplyReport, DestCollision, DriftEntry, SourceResult, apply, detect_drift,
    is_floating_rev,
};
pub use error::AgentsError;
pub use manifest::{DirMapping, FileMapping, FileMode, UpstreamManifest, parse_manifest};
pub use merge::{ResolvedEntry, expand_source};
pub use state::{AgentsState, AppliedFile, SourceRecord};
