//! `[agents]` configuration: external repos that ship agent
//! instructions and skills (CLAUDE.md, AGENTS.md, .claude/skills/, …)
//! plus per-source overrides applied on top of the upstream manifest.
//!
//! This module owns only the wire format and structural validation.
//! The actual cache + apply pipeline lives in `keel-agents`.

use serde::Deserialize;

const DEFAULT_MANIFEST_PATH: &str = "keel-agents.toml";

fn default_manifest_path() -> String {
    DEFAULT_MANIFEST_PATH.to_string()
}

const fn true_default() -> bool {
    true
}

/// Top-level `[agents]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// When true (default), `keel install` runs `keel agents
    /// install` as a synthetic step before the git-hook step. Mirrors
    /// the `[install].install_git_hooks` flag.
    #[serde(default = "true_default")]
    pub install_with_setup: bool,

    /// Default upstream manifest path, used when a source doesn't
    /// supply its own. Resolves inside the upstream clone (after any
    /// `subpath` is applied).
    #[serde(default = "default_manifest_path")]
    pub manifest_path: String,

    /// Upstream sources, in declaration order. When two sources resolve
    /// the same destination, the later-declared one wins (with a
    /// warning).
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            install_with_setup: true,
            manifest_path: default_manifest_path(),
            sources: Vec::new(),
        }
    }
}

/// One upstream source feeding the agents apply pipeline.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    /// Stable identifier used in state, status output, and
    /// `agents update --source <name>`. Must be unique across sources.
    pub name: String,

    /// Git URL passed straight to `git clone`. Filesystem paths work
    /// too — useful for tests and for org-internal mirrors.
    pub repo: String,

    /// Pinned revision: tag, branch, or full SHA. Floating refs (e.g.
    /// `main`) are allowed but get auto-refetched on every `agents
    /// update`.
    pub rev: String,

    /// Optional subpath inside the upstream clone. Manifest lookup and
    /// every `[[file]]`/`[[dir]]` `src` are resolved relative to this
    /// directory. Useful for monorepos that keep agent rules in a
    /// subdir.
    #[serde(default)]
    pub subpath: Option<String>,

    /// Per-source override of [`AgentsConfig::manifest_path`].
    #[serde(default)]
    pub manifest_path: Option<String>,

    /// Mapping overrides applied on top of the upstream manifest.
    /// Match key is the upstream-declared destination — that's the
    /// path the user actually sees in their tree.
    #[serde(default)]
    pub overrides: Vec<MappingOverride>,
}

/// Override applied to a single upstream-declared mapping. Exactly one
/// of `action = "skip"` and `relocate = "<new dest>"` must be set;
/// validation enforces this at config-load time.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MappingOverride {
    /// Upstream destination path that selects which mapping to override.
    pub dest: String,

    /// Drop the mapping entirely.
    #[serde(default)]
    pub action: Option<MappingOverrideKind>,

    /// Rewrite the destination to this path.
    #[serde(default)]
    pub relocate: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MappingOverrideKind {
    Skip,
}

/// Validated form of a [`MappingOverride`]. Use
/// [`MappingOverride::resolved`] to convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedOverride {
    Skip,
    Relocate(String),
}

impl MappingOverride {
    pub fn resolved(&self) -> Result<ResolvedOverride, String> {
        match (&self.action, &self.relocate) {
            (Some(MappingOverrideKind::Skip), None) => Ok(ResolvedOverride::Skip),
            (None, Some(dest)) if !dest.is_empty() => Ok(ResolvedOverride::Relocate(dest.clone())),
            (None, None) => Err(format!(
                "agents.sources.overrides[dest=\"{}\"]: must set either `action = \"skip\"` or `relocate = \"<path>\"`",
                self.dest
            )),
            (Some(_), Some(_)) => Err(format!(
                "agents.sources.overrides[dest=\"{}\"]: `action` and `relocate` are mutually exclusive",
                self.dest
            )),
            (None, Some(_)) => Err(format!(
                "agents.sources.overrides[dest=\"{}\"]: `relocate` must be a non-empty path",
                self.dest
            )),
        }
    }
}

impl AgentsConfig {
    /// Structural validation: source-name uniqueness + per-override
    /// shape. Called from [`crate::config::Config::validate`].
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for source in &self.sources {
            if source.name.is_empty() {
                return Err("agents.sources entry is missing `name`".to_string());
            }
            if !seen.insert(source.name.as_str()) {
                return Err(format!("duplicate agents source name `{}`", source.name));
            }
            if source.repo.is_empty() {
                return Err(format!(
                    "agents.sources[name=\"{}\"]: `repo` must not be empty",
                    source.name
                ));
            }
            if source.rev.is_empty() {
                return Err(format!(
                    "agents.sources[name=\"{}\"]: `rev` must not be empty",
                    source.name
                ));
            }
            for ov in &source.overrides {
                ov.resolved()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use pretty_assertions::assert_eq;

    #[test]
    fn defaults_when_section_absent() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.agents.install_with_setup);
        assert_eq!(cfg.agents.manifest_path, DEFAULT_MANIFEST_PATH);
        assert!(cfg.agents.sources.is_empty());
    }

    #[test]
    fn parses_full_section() {
        let src = r#"
            [agents]
            install_with_setup = false
            manifest_path = "custom-agents.toml"

            [[agents.sources]]
            name = "baseline"
            repo = "https://example.com/baseline.git"
            rev  = "v1.4.0"
            subpath = "claude/"

            [[agents.sources.overrides]]
            dest   = "AGENTS.md"
            action = "skip"

            [[agents.sources.overrides]]
            dest     = ".claude/skills/sec.md"
            relocate = ".claude/skills/sec.upstream.md"

            [[agents.sources]]
            name = "rust"
            repo = "https://example.com/rust.git"
            rev  = "main"
            manifest_path = "alt.toml"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(!cfg.agents.install_with_setup);
        assert_eq!(cfg.agents.manifest_path, "custom-agents.toml");
        assert_eq!(cfg.agents.sources.len(), 2);

        let baseline = &cfg.agents.sources[0];
        assert_eq!(baseline.name, "baseline");
        assert_eq!(baseline.subpath.as_deref(), Some("claude/"));
        assert!(baseline.manifest_path.is_none());
        assert_eq!(baseline.overrides.len(), 2);
        assert_eq!(
            baseline.overrides[0].resolved().unwrap(),
            ResolvedOverride::Skip
        );
        assert_eq!(
            baseline.overrides[1].resolved().unwrap(),
            ResolvedOverride::Relocate(".claude/skills/sec.upstream.md".into())
        );

        let rust = &cfg.agents.sources[1];
        assert_eq!(rust.manifest_path.as_deref(), Some("alt.toml"));
    }

    #[test]
    fn validate_rejects_duplicate_source_names() {
        let cfg: Config = toml::from_str(
            r#"
            [[agents.sources]]
            name = "x"
            repo = "https://example.com/a.git"
            rev  = "v1"

            [[agents.sources]]
            name = "x"
            repo = "https://example.com/b.git"
            rev  = "v1"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("duplicate agents source name `x`"));
    }

    #[test]
    fn validate_rejects_empty_rev() {
        let cfg: Config = toml::from_str(
            r#"
            [[agents.sources]]
            name = "x"
            repo = "https://example.com/a.git"
            rev  = ""
        "#,
        )
        .unwrap();
        assert!(cfg.validate().unwrap_err().contains("`rev`"));
    }

    #[test]
    fn override_requires_exactly_one_action() {
        let none = MappingOverride {
            dest: "X.md".into(),
            action: None,
            relocate: None,
        };
        assert!(none.resolved().is_err());
        let both = MappingOverride {
            dest: "X.md".into(),
            action: Some(MappingOverrideKind::Skip),
            relocate: Some("Y.md".into()),
        };
        assert!(both.resolved().is_err());
    }
}
