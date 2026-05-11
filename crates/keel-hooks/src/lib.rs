//! Git hook installation and pre-commit-config compatibility.
//!
//! Bounded context: anything related to running staged-file checks at git
//! lifecycle points. Four concerns kept separable:
//!
//! - [`config`] parses `.pre-commit-config.yaml` plus the upstream
//!   `.pre-commit-hooks.yaml` shipped inside external hook repos.
//! - [`cache`] clones external hook repos into `.keel/cache/hooks/`
//!   and remembers the resolved revision sha. keel owns this cache
//!   end-to-end; it never delegates to the `pre-commit` binary.
//! - [`runner`] runs the hooks for a given stage. Native execution
//!   covers `repo: local` and cached external repos when their
//!   resolved `language` is `system | script`. Anything else
//!   (`python`, `node`, `repo: meta`, …) is rejected with a clear
//!   error rather than silently skipping.
//! - [`installer`] writes `.git/hooks/<stage>` shims that delegate to
//!   `keel hooks run <stage> "$@"`.
//!
//! Native keel hooks (declared in `keel.toml` `[hooks]`) are run by
//! the CLI through [`keel_runtime::Executor`], not by this crate — they
//! reuse the recipe runner. This crate handles the pre-commit ecosystem
//! and the git-hooks plumbing.

pub mod cache;
pub mod config;
pub mod error;
pub mod git;
pub mod installer;
pub mod runner;

pub use cache::{CacheMeta, CachedRepo, cache_root, clone_or_reuse};
pub use config::{HookLanguage, HookSpec, PreCommitConfig, Repo, UpstreamHook};
pub use error::HookError;
pub use installer::{KEEL_HOOK_MARKER, install, install_one, uninstall};
pub use runner::{HookOutcome, run_pre_commit};
pub use keel_cache::CacheError;
