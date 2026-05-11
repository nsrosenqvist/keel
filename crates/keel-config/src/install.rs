//! `[install]` configuration and `.keel/install/` step discovery.
//!
//! The install flow has two surfaces:
//!
//! - **Declarative** — a top-level `[install]` table that names an
//!   ordered sequence of steps. Each entry is either the name of a
//!   recipe / discovered step or an inline `{ run = "…", … }` table.
//! - **Discovered** — shell files under `.keel/install/` are picked up
//!   automatically, sorted by file name. When `[install].steps` is
//!   unset, the discovered list *is* the install plan, which is enough
//!   for projects that prefer to author every step as a script.
//!
//! Install steps are deliberately kept **out** of [`crate::Config::commands`]
//! and [`crate::Config::scripts`]: they should not surface in
//! `keel list` or the TUI sidebar. The author can still reference a
//! regular recipe from `[install].steps` if they want a step to also be
//! runnable interactively — that's an explicit opt-in.

use crate::error::ConfigError;
use crate::scripts::ScriptCommand;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level `[install]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstallConfig {
    /// Ordered list of step references. When empty, the install plan
    /// is the alphabetical listing of `.keel/install/*` instead.
    #[serde(default)]
    pub steps: Vec<InstallStepRef>,

    /// When true (default), an "install-hooks" pseudo-step runs after
    /// the user-defined steps to install git-hook shims and prefetch
    /// any external hook repos referenced by `.pre-commit-config.yaml`.
    /// Set `false` if the project doesn't want hooks managed by keel.
    #[serde(default = "true_default")]
    pub install_git_hooks: bool,

    /// Path of the auto-managed `.gitignore`. Relative paths resolve
    /// against the project root. The default value is intentionally
    /// inside `.keel/` so it only governs keel-owned files and
    /// can't accidentally shadow the project's own root `.gitignore`.
    #[serde(default = "default_gitignore")]
    pub gitignore: String,

    /// Steps discovered under `.keel/install/`. Populated by the
    /// loader after the TOML deserialisation pass; never serialised.
    #[serde(skip)]
    pub discovered: BTreeMap<String, InstallStepScript>,

    /// Source-of-truth order for [`Self::discovered`]: the file-name
    /// sort the directory scan produced. `BTreeMap` already gives an
    /// alphabetical iter, but step file names typically use numeric
    /// prefixes (`01-copy-env`, `02-composer`) where the BTree order
    /// happens to coincide; we keep this list anyway so future code
    /// can swap to a non-lexicographic ordering without breaking the
    /// contract.
    #[serde(skip)]
    pub discovered_order: Vec<String>,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            install_git_hooks: true,
            gitignore: default_gitignore(),
            discovered: BTreeMap::new(),
            discovered_order: Vec::new(),
        }
    }
}

const fn true_default() -> bool {
    true
}

fn default_gitignore() -> String {
    ".keel/.gitignore".to_string()
}

/// One entry in `[install].steps`.
///
/// `Name` is the common case: the step *is* a recipe / discovered file
/// referenced by name. `Inline` is for one-off shell that doesn't
/// warrant its own file.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum InstallStepRef {
    /// Reference by name. Resolved at runtime against
    /// `[command.*]`, [`InstallConfig::discovered`], and (for
    /// projects that explicitly opted in) `Config.scripts`.
    Name(String),
    /// Inline step. Avoids a separate file when the step is one or
    /// two lines of shell.
    Inline(InlineStep),
}

/// Inline step embedded directly in `[install].steps`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InlineStep {
    /// Display name used in the renderer row and the state file.
    /// Required so a failed step can be referenced by `keel install
    /// <name>` on retry.
    pub name: String,

    /// Shell command to run. Single string form only; multi-step
    /// inline blocks belong in a `.keel/install/<name>` file or a
    /// regular recipe.
    pub run: String,

    #[serde(default)]
    pub desc: Option<String>,

    /// Service name to exec inside (host process when absent).
    #[serde(default, rename = "in")]
    pub service: Option<String>,

    #[serde(default)]
    pub tty: bool,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Working directory the step runs in. Relative paths resolve
    /// against the project root.
    #[serde(default)]
    pub cwd: Option<String>,

    /// When true, a non-zero exit logs a warning and the install
    /// continues. Useful for "nice to have" steps that shouldn't gate
    /// the rest of the setup.
    #[serde(default)]
    pub optional: bool,

    /// When true, the renderer hands the terminal directly to the
    /// step (inherited stdio) for the duration of its run, then
    /// resumes drawing afterwards. Set for steps that prompt via
    /// `keel lib ask | confirm | password | select | filter`.
    #[serde(default)]
    pub interactive: bool,
}

/// A step discovered under `.keel/install/`. Same frontmatter parser
/// as [`ScriptCommand`] — install-specific keys (`cwd`, `optional`,
/// `interactive`) are accepted on either kind so authors don't have to
/// memorise which keys live where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStepScript {
    pub name: String,
    pub path: PathBuf,
    pub desc: Option<String>,
    pub service: Option<String>,
    pub tty: bool,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub optional: bool,
    pub interactive: bool,
}

impl From<ScriptCommand> for InstallStepScript {
    fn from(cmd: ScriptCommand) -> Self {
        Self {
            name: cmd.name,
            path: cmd.path,
            desc: cmd.desc,
            service: cmd.service,
            tty: cmd.tty,
            env: cmd.env,
            cwd: cmd.cwd,
            optional: cmd.optional,
            interactive: cmd.interactive,
        }
    }
}

/// Scan `.keel/install/` for step files. Same skip rules as the
/// `.keel/commands/` scan (hidden / underscore-prefixed files are
/// ignored, non-regular files skipped). Returns the steps keyed by
/// name plus the alphabetical filename order so the runner can iterate
/// without re-sorting.
pub fn discover_install_steps(
    dir: &Path,
) -> Result<(BTreeMap<String, InstallStepScript>, Vec<String>), ConfigError> {
    let mut out = BTreeMap::new();
    let mut order = Vec::new();

    let entries = std::fs::read_dir(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    // Collect filenames first so we can sort before parsing — keeps
    // the public ordering deterministic across platforms.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| ConfigError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str.starts_with('_') {
            continue;
        }
        candidates.push(entry.path());
    }
    candidates.sort();

    for path in candidates {
        let cmd = ScriptCommand::from_path(&path)?;
        order.push(cmd.name.clone());
        out.insert(cmd.name.clone(), cmd.into());
    }
    Ok((out, order))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn install_section_defaults_when_absent() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.install.steps.is_empty());
        assert!(cfg.install.install_git_hooks);
        assert_eq!(cfg.install.gitignore, ".keel/.gitignore");
        assert!(cfg.install.discovered.is_empty());
    }

    #[test]
    fn parses_named_steps() {
        let cfg: Config = toml::from_str(
            r#"
            [install]
            steps = ["copy-env", "composer-install", "migrate-fresh"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.install.steps.len(), 3);
        match &cfg.install.steps[0] {
            InstallStepRef::Name(n) => assert_eq!(n, "copy-env"),
            other => panic!("expected Name, got {other:?}"),
        }
    }

    #[test]
    fn parses_inline_step() {
        let cfg: Config = toml::from_str(
            r#"
            [[install.steps]]
            name = "copy-env"
            run = "test -f .env || cp .env.example .env"
            desc = "Seed .env from example"
            "#,
        )
        .unwrap();
        match &cfg.install.steps[0] {
            InstallStepRef::Inline(step) => {
                assert_eq!(step.name, "copy-env");
                assert_eq!(step.run, "test -f .env || cp .env.example .env");
                assert_eq!(step.desc.as_deref(), Some("Seed .env from example"));
                assert!(!step.optional);
                assert!(!step.interactive);
            }
            other => panic!("expected Inline, got {other:?}"),
        }
    }

    #[test]
    fn parses_mixed_named_and_inline_steps() {
        let cfg: Config = toml::from_str(
            r#"
            [install]
            install_git_hooks = false

            [[install.steps]]
            name = "first"
            run = "echo first"

            [[install.steps]]
            name = "second"
            run = "echo second"
            optional = true
            interactive = true
            "#,
        )
        .unwrap();
        assert!(!cfg.install.install_git_hooks);
        assert_eq!(cfg.install.steps.len(), 2);
        match &cfg.install.steps[1] {
            InstallStepRef::Inline(step) => {
                assert!(step.optional);
                assert!(step.interactive);
            }
            other => panic!("expected Inline, got {other:?}"),
        }
    }

    #[test]
    fn discover_install_steps_sorts_alphabetically() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("02-composer"),
            "#!/bin/sh\n# @desc: Install dependencies\ncomposer install\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("01-copy-env"),
            "#!/bin/sh\n# @optional: yes\ncp .env.example .env\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("03-migrate"),
            "#!/bin/sh\n# @interactive: true\necho hi\n",
        )
        .unwrap();

        let (map, order) = discover_install_steps(dir.path()).unwrap();
        assert_eq!(order, vec!["01-copy-env", "02-composer", "03-migrate"]);
        assert!(map["01-copy-env"].optional);
        assert!(map["03-migrate"].interactive);
        assert_eq!(
            map["02-composer"].desc.as_deref(),
            Some("Install dependencies")
        );
    }

    #[test]
    fn discover_install_steps_skips_hidden_and_underscored() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".hidden"), "echo nope\n").unwrap();
        std::fs::write(dir.path().join("_helper"), "echo nope\n").unwrap();
        std::fs::write(dir.path().join("real"), "echo yes\n").unwrap();
        let (map, order) = discover_install_steps(dir.path()).unwrap();
        assert_eq!(order, vec!["real"]);
        assert_eq!(map.len(), 1);
    }
}
