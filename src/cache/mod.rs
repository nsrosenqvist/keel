//! Content-addressed git cache used by `ampelos-hooks` and
//! `ampelos-agents`. See [`git`] for the surface and on-disk layout.

pub mod error;
pub mod git;

pub use error::CacheError;
pub use git::{CacheKind, CacheMeta, CachedRepo, RepoRef, cache_root, clone_or_reuse};
