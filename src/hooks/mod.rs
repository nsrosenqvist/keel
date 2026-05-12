//! Git hook installation and pre-commit-config compatibility.
//!
//! Bounded context: anything related to running staged-file checks at git
//! lifecycle points. Four concerns kept separable:
//!
//! - [`config`] parses `.pre-commit-config.yaml` plus the upstream
//!   `.pre-commit-hooks.yaml` shipped inside external hook repos.
//! - [`cache`] clones external hook repos into `.ampelos/cache/hooks/`
//!   and remembers the resolved revision sha. ampelos owns this cache
//!   end-to-end; it never delegates to the `pre-commit` binary.
//! - [`runner`] runs the hooks for a given stage. Each hook's `entry`
//!   is exec'd verbatim regardless of the declared `language` — ampelos
//!   trusts the user to have the runtime on `PATH` (typically via
//!   `ampelos install`) rather than reimplementing pre-commit's
//!   virtualenv / toolchain manager. `repo: meta` is the one shape
//!   that's still rejected at run time, since there's no `entry` to
//!   dispatch. Per-hook `in = "<service>"` and the executor's
//!   workspace target (devcontainer when enabled, host otherwise)
//!   pick where the spawn lands.
//! - [`installer`] writes `.git/hooks/<stage>` shims that delegate to
//!   `ampelos hooks run <stage> "$@"`.
//!
//! Native ampelos hooks (declared in `ampelos.toml` `[hooks]`) are run by
//! the CLI through [`crate::runtime::Executor`], not by this crate — they
//! reuse the recipe runner. This crate handles the pre-commit ecosystem
//! and the git-hooks plumbing.

pub mod cache;
pub mod config;
pub mod error;
pub mod git;
pub mod installer;
pub mod runner;

pub use crate::cache::CacheError;
pub use cache::{CacheMeta, CachedRepo, cache_root, clone_or_reuse};
pub use config::{HookLanguage, HookSpec, PreCommitConfig, Repo, UpstreamHook};
pub use error::HookError;
pub use installer::{AMPELOS_HOOK_MARKER, install, install_one, uninstall};
pub use runner::{HookOutcome, run_pre_commit};
