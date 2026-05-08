//! `.pre-commit-config.yaml` schema.
//!
//! The schema scaffl reads is a strict subset of pre-commit's. Unknown keys
//! are tolerated (forward compat with the upstream format), unsupported
//! semantics are flagged at run-time by the [`runner`](crate::runner)
//! rather than at parse time. This keeps `scaffl hooks list` working even
//! when the config references hooks scaffl can't run natively yet.

use crate::error::HookError;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PreCommitConfig {
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default, rename = "default_stages")]
    pub default_stages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Repo {
    pub repo: String,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
}

impl Repo {
    pub fn is_local(&self) -> bool {
        self.repo == "local" || self.repo == "meta"
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HookSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_language")]
    pub language: HookLanguage,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Regex matched against staged file paths.
    #[serde(default)]
    pub files: Option<String>,
    /// Regex; matching files are excluded after `files` filtering.
    #[serde(default)]
    pub exclude: Option<String>,
    #[serde(default = "default_pass_filenames")]
    pub pass_filenames: bool,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub always_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookLanguage {
    System,
    Script,
    Python,
    Node,
    Ruby,
    Golang,
    Rust,
    Docker,
    DockerImage,
    Pygrep,
    Fail,
    #[serde(other)]
    Other,
}

impl HookLanguage {
    /// Whether scaffl can run this language natively (no virtualenv etc.).
    pub fn is_native(&self) -> bool {
        matches!(self, HookLanguage::System | HookLanguage::Script)
    }
}

fn default_language() -> HookLanguage {
    HookLanguage::System
}

const fn default_pass_filenames() -> bool {
    true
}

impl HookSpec {
    /// Whether this hook applies to `stage`.
    ///
    /// Pre-commit treats an empty `stages` list as "all stages", and
    /// project-level `default_stages` provides a fallback. We mirror that.
    pub fn applies_to_stage(&self, stage: &str, default_stages: &[String]) -> bool {
        if self.stages.is_empty() {
            return default_stages.is_empty() || default_stages.iter().any(|s| s == stage);
        }
        self.stages.iter().any(|s| s == stage)
    }
}

/// Read a `.pre-commit-config.yaml` from disk.
pub fn load_from_path(path: &Path) -> Result<PreCommitConfig, HookError> {
    let raw = std::fs::read_to_string(path).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_str(&raw).map_err(|source| HookError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(yaml: &str) -> PreCommitConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn parses_local_system_hook() {
        let cfg = parse(
            r#"
            repos:
              - repo: local
                hooks:
                  - id: rust-fmt
                    name: Rust fmt
                    language: system
                    entry: cargo fmt --check
                    files: \.rs$
                    pass_filenames: false
            "#,
        );
        assert_eq!(cfg.repos.len(), 1);
        let repo = &cfg.repos[0];
        assert!(repo.is_local());
        let hook = &repo.hooks[0];
        assert_eq!(hook.id, "rust-fmt");
        assert_eq!(hook.language, HookLanguage::System);
        assert_eq!(hook.entry.as_deref(), Some("cargo fmt --check"));
        assert!(!hook.pass_filenames);
        assert!(hook.language.is_native());
    }

    #[test]
    fn unknown_language_falls_back_to_other() {
        let cfg = parse(
            r#"
            repos:
              - repo: local
                hooks:
                  - id: weird
                    language: brainfuck
                    entry: noop
            "#,
        );
        assert_eq!(cfg.repos[0].hooks[0].language, HookLanguage::Other);
    }

    #[test]
    fn parses_external_repo_with_rev() {
        let cfg = parse(
            r#"
            repos:
              - repo: https://github.com/pre-commit/pre-commit-hooks
                rev: v4.5.0
                hooks:
                  - id: trailing-whitespace
            "#,
        );
        assert!(!cfg.repos[0].is_local());
        assert_eq!(cfg.repos[0].rev.as_deref(), Some("v4.5.0"));
    }

    #[test]
    fn pass_filenames_defaults_true() {
        let cfg = parse(
            r#"
            repos:
              - repo: local
                hooks:
                  - id: x
                    language: system
                    entry: echo
            "#,
        );
        assert!(cfg.repos[0].hooks[0].pass_filenames);
    }

    #[test]
    fn stage_filter_with_no_stages_uses_defaults() {
        let hook = HookSpec {
            id: "x".into(),
            name: None,
            language: HookLanguage::System,
            entry: None,
            args: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            stages: vec![],
            always_run: false,
        };
        assert!(hook.applies_to_stage("pre-commit", &[]));
        assert!(hook.applies_to_stage("pre-commit", &["pre-commit".into()]));
        assert!(!hook.applies_to_stage("pre-push", &["pre-commit".into()]));
    }

    #[test]
    fn stage_filter_explicit() {
        let hook = HookSpec {
            id: "x".into(),
            name: None,
            language: HookLanguage::System,
            entry: None,
            args: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            stages: vec!["pre-push".into()],
            always_run: false,
        };
        assert!(!hook.applies_to_stage("pre-commit", &[]));
        assert!(hook.applies_to_stage("pre-push", &[]));
    }
}
