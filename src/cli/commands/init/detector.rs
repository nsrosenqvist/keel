//! Detector trait + the fragment vocabulary detectors contribute to the
//! generated `ampelos.toml`.
//!
//! Each detector is a zero-sized struct implementing [`Detector`] and lives
//! in `detectors/<ecosystem>.rs`. The orchestration in [`super::run`] walks
//! the registry, collects every `Some(Finding)`, and hands the lot to the
//! renderer. There is no global mutable state and no detector-to-detector
//! communication — section ownership is enforced in the renderer.

use std::path::{Path, PathBuf};

/// A detector for a single project signal (a language ecosystem, a
/// container runtime, a stray dotenv file, etc.). Stateless and `Sync` so
/// the registry can be a `&'static [&'static dyn Detector]`.
pub trait Detector: Sync {
    /// Inspect `root` and return a [`Finding`] when this ecosystem
    /// applies. Filesystem reads only; no network, no parsing of
    /// project-internal files (`package.json` scripts, etc.) — that is
    /// out of scope for the initial detection pass.
    fn detect(&self, root: &Path) -> Option<Finding>;
}

/// What one detector contributes when it matches.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Short ecosystem label (`"node"`, `"compose"`, …) used in the
    /// "multiple ecosystems suggest X" header for duplicate command
    /// names.
    pub ecosystem: &'static str,
    /// Specific tool chosen inside the ecosystem (`"pnpm"`, `"uv"`, …).
    /// Currently a verification surface for per-detector tool-selection
    /// tests; not consumed by the renderer.
    #[allow(dead_code)]
    pub tool: Option<String>,
    pub fragments: Vec<Fragment>,
    /// Human-readable lines printed to stdout after the file is written.
    pub notes: Vec<String>,
}

/// A single piece of TOML the renderer will weave into the output. Each
/// variant maps to one section of [`crate::config::model::Config`].
#[derive(Debug, Clone)]
pub enum Fragment {
    /// `[runtime]` — owned exclusively by container detectors. Renderer
    /// uses the first one it sees; subsequent ones are reported as
    /// notes and discarded.
    Runtime {
        backend: &'static str,
        default_service: Option<String>,
        compose_passthrough: bool,
        service_passthrough: bool,
    },
    /// `[devcontainer]` — owned exclusively by the devcontainer detector.
    Devcontainer { path: PathBuf },
    /// One file added to `[env_files] files`. Deduplicated across detectors.
    EnvFile(String),
    /// `[command.<name>]` — multiple detectors may emit the same name;
    /// the renderer groups them under a "pick one" header in that case.
    Command(CommandFragment),
}

/// Field-by-field description of a single `[command.<name>]` block.
/// Mirrors a useful subset of [`crate::config::model::Recipe`] — anything
/// not represented here (env, parallel, profiles) is out of scope for
/// auto-detected suggestions.
#[derive(Debug, Clone)]
pub struct CommandFragment {
    pub name: String,
    pub desc: String,
    pub run: RunSpec,
    pub in_service: Option<String>,
    pub tty: Option<bool>,
    pub forward_args: Option<bool>,
    pub needs: Vec<String>,
}

/// Locally-owned twin of [`crate::config::model::Run`] — kept separate
/// because that type is `Deserialize`-only and untagged, awkward to
/// construct in generation code.
#[derive(Debug, Clone)]
pub enum RunSpec {
    Single(String),
    Steps(Vec<String>),
}

impl CommandFragment {
    /// Convenience for the common single-line shell command shape.
    pub fn shell(name: impl Into<String>, desc: impl Into<String>, run: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            desc: desc.into(),
            run: RunSpec::Single(run.into()),
            in_service: None,
            tty: None,
            forward_args: None,
            needs: Vec::new(),
        }
    }

    pub fn with_forward_args(mut self) -> Self {
        self.forward_args = Some(true);
        self
    }

    pub fn with_tty(mut self) -> Self {
        self.tty = Some(true);
        self
    }
}
