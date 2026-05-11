//! `.pre-commit-config.yaml` schema.
//!
//! The schema keel reads is a strict subset of pre-commit's. Unknown keys
//! are tolerated (forward compat with the upstream format), unsupported
//! semantics are flagged at run-time by the [`runner`](crate::hooks::runner)
//! rather than at parse time. This keeps `keel hooks list` working even
//! when the config references hooks keel can't run natively yet.

use crate::hooks::error::HookError;
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
    /// True when the repo's hooks are defined inline in the user's
    /// `.pre-commit-config.yaml` (no clone needed). `repo: meta` was
    /// historically treated as local; it isn't — meta references
    /// pre-commit's built-in lint hooks, which keel does not
    /// implement. The runner errors when it encounters one.
    pub fn is_local(&self) -> bool {
        self.repo == "local"
    }

    /// True only for `repo: meta` — pre-commit's built-in hook bundle.
    pub fn is_meta(&self) -> bool {
        self.repo == "meta"
    }
}

/// Single hook entry as it appears inside an upstream repo's
/// `.pre-commit-hooks.yaml`. The shape matches [`HookSpec`] from the
/// user-side config — `id` is required, every other field is a
/// fallback that the user may override in their own
/// `.pre-commit-config.yaml`.
///
/// Kept as a separate struct (rather than reusing [`HookSpec`]) because
/// the merge direction matters: the user's `HookSpec` is the source of
/// truth for everything they explicitly set, falling back to this only
/// where they were silent.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct UpstreamHook {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub language: Option<HookLanguage>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub files: Option<String>,
    #[serde(default)]
    pub exclude: Option<String>,
    #[serde(default)]
    pub pass_filenames: Option<bool>,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub always_run: Option<bool>,
}

/// Read the `.pre-commit-hooks.yaml` from the root of a cached upstream
/// repo. Returns the full list as authored — callers index by `id`.
pub fn load_upstream_hooks(repo_dir: &Path) -> Result<Vec<UpstreamHook>, HookError> {
    let path = repo_dir.join(".pre-commit-hooks.yaml");
    let raw = std::fs::read_to_string(&path).map_err(|source| HookError::Io {
        path: path.clone(),
        source,
    })?;
    serde_yaml_ng::from_str(&raw).map_err(|source| HookError::Parse { path, source })
}

/// Merge an upstream hook definition into the user's reference. The
/// user wins on every explicitly-set field; upstream fills the gaps.
/// Errors when the merged result still lacks an `entry` or has a
/// non-native language — those translate to clear install-time errors.
pub fn merge_with_upstream(
    user: &HookSpec,
    upstream: &UpstreamHook,
) -> Result<HookSpec, HookError> {
    // Upstream owns the language field — that's where the hook was
    // authored. We fall back to the user's value only when upstream
    // didn't declare one (rare, since `.pre-commit-hooks.yaml`
    // requires it).
    let language = upstream
        .language
        .clone()
        .unwrap_or_else(|| user.language.clone());
    if !language.is_native() {
        return Err(HookError::UnsupportedLanguage {
            hook: user.id.clone(),
            language: format!("{language:?}"),
        });
    }
    let entry = user
        .entry
        .clone()
        .or_else(|| upstream.entry.clone())
        .ok_or_else(|| HookError::EntryMissing {
            hook: user.id.clone(),
        })?;
    Ok(HookSpec {
        id: user.id.clone(),
        name: user.name.clone().or_else(|| upstream.name.clone()),
        language,
        entry: Some(entry),
        args: if user.args.is_empty() {
            upstream.args.clone()
        } else {
            user.args.clone()
        },
        files: user.files.clone().or_else(|| upstream.files.clone()),
        exclude: user.exclude.clone().or_else(|| upstream.exclude.clone()),
        // `pass_filenames` defaults to true at deserialization, so we
        // can't tell whether the user said "yes" or omitted it. The
        // upstream value, when present, is taken as the more
        // authoritative default — but only when the user kept the
        // default true. Users who say `pass_filenames: false`
        // explicitly are honoured.
        pass_filenames: if !user.pass_filenames {
            false
        } else {
            upstream.pass_filenames.unwrap_or(true)
        },
        stages: if user.stages.is_empty() {
            upstream.stages.clone()
        } else {
            user.stages.clone()
        },
        always_run: user.always_run || upstream.always_run.unwrap_or(false),
    })
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
    /// Whether keel can run this language natively (no virtualenv etc.).
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
