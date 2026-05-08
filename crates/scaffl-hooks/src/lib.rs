//! Git hook installation and pre-commit-config compatibility.
//!
//! Bounded context: anything related to running staged-file checks at git
//! lifecycle points. Three concerns kept separable:
//!
//! - [`config`] parses `.pre-commit-config.yaml` into a typed model.
//! - [`runner`] runs the hooks for a given stage. Native execution covers
//!   `repo: local` with `language: system | script`. Other languages and
//!   external repos bridge to the `pre-commit` binary if it's on PATH;
//!   otherwise they're skipped with a warning so the rest of the stage can
//!   still complete.
//! - [`installer`] writes `.git/hooks/<stage>` shims that delegate to
//!   `scaffl hooks run <stage> "$@"`.
//!
//! Native scaffl hooks (declared in `scaffl.toml` `[hooks]`) are run by
//! the CLI through [`scaffl_runtime::Executor`], not by this crate — they
//! reuse the recipe runner. This crate handles the pre-commit ecosystem
//! and the git-hooks plumbing.

pub mod config;
pub mod error;
pub mod git;
pub mod installer;
pub mod runner;

pub use config::{HookLanguage, HookSpec, PreCommitConfig, Repo};
pub use error::HookError;
pub use installer::{SCAFFL_HOOK_MARKER, install, install_one, uninstall};
pub use runner::{HookOutcome, run_pre_commit};
