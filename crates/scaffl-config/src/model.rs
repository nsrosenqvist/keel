//! Domain model for `scaffl.toml`.
//!
//! Types here are value objects: they are constructed from the TOML source,
//! validated, and then handed (immutable) to the runtime. They expose no
//! behaviour beyond accessors. Behaviour lives in `scaffl-runtime`.

use crate::scripts::ScriptCommand;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Top-level configuration loaded from a project's `scaffl.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub project: ProjectConfig,

    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Project-level environment variable defaults and resolution rules.
    #[serde(default)]
    pub env: BTreeMap<String, EnvSpec>,

    #[serde(default)]
    pub env_files: EnvFilesConfig,

    /// Recipes keyed by name. TOML expresses these as `[command.<name>]`,
    /// flattened into this map after deserialization.
    #[serde(default, rename = "command")]
    pub commands: BTreeMap<String, Recipe>,

    #[serde(default)]
    pub ui: UiConfig,

    /// Scripts discovered under `.scaffl/commands/`. Populated by
    /// [`crate::loader::load_project`]; never serialized.
    #[serde(skip)]
    pub scripts: BTreeMap<String, ScriptCommand>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "default_backend")]
    pub backend: Backend,

    #[serde(default)]
    pub default_service: Option<String>,

    #[serde(default = "true_default")]
    pub compose_passthrough: bool,

    #[serde(default = "true_default")]
    pub service_passthrough: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            default_service: None,
            compose_passthrough: true,
            service_passthrough: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Compose,
    Docker,
    Podman,
    None,
}

fn default_backend() -> Backend {
    Backend::Compose
}

const fn true_default() -> bool {
    true
}

/// Specification for a single environment variable.
///
/// Values resolve in order: explicit `value` → `from_command` output →
/// existing env → `default`. Required-but-missing values produce a
/// `ConfigError::Invalid` at runtime, not at parse time, so missing
/// secrets don't break `scaffl list` or `scaffl which`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvSpec {
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub from_command: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvFilesConfig {
    #[serde(default)]
    pub files: Vec<String>,
}

/// A single recipe (`[command.<name>]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    #[serde(default)]
    pub desc: Option<String>,

    /// What to run. Either a single shell-parsed string or a list of steps.
    /// Multi-step logic beyond a flat list belongs in `.scaffl/commands/`.
    pub run: Run,

    /// Service to exec inside (via the configured backend). Absent → host.
    #[serde(default, rename = "in")]
    pub service: Option<String>,

    #[serde(default)]
    pub tty: bool,

    /// Per-recipe env overrides applied last in the merge chain.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Other recipes that must succeed before this one runs.
    #[serde(default)]
    pub needs: Vec<String>,

    /// When true, additional CLI args are appended to the command.
    #[serde(default)]
    pub forward_args: bool,

    /// When `run` is an array, run steps concurrently rather than sequentially.
    #[serde(default)]
    pub parallel: bool,
}

/// Either a single command string or a sequence of steps.
///
/// Encoded as `untagged` so TOML may write either `run = "..."` or
/// `run = [...]` without a discriminator.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Run {
    Single(String),
    Steps(Vec<String>),
}

impl Run {
    /// Number of steps, where a single command counts as one.
    pub fn len(&self) -> usize {
        match self {
            Run::Single(_) => 1,
            Run::Steps(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Run::Steps(v) if v.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    #[serde(default)]
    pub default: Option<String>,

    #[serde(default, rename = "pane")]
    pub panes: Vec<UiPane>,
}

/// A predefined pane shown in the TUI dashboard.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum UiPane {
    Service {
        service: String,
        #[serde(default)]
        key: Option<String>,
    },
    Command {
        name: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        restart_on: Vec<String>,
    },
    Watcher {
        glob: Vec<String>,
        on_change: String,
        #[serde(default = "default_debounce")]
        debounce_ms: u64,
        #[serde(default)]
        key: Option<String>,
    },
}

const fn default_debounce() -> u64 {
    300
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_minimal_config() {
        let src = r#"
            [project]
            name = "myapp"

            [command.up]
            run = "docker compose up -d"
        "#;

        let cfg: Config = toml::from_str(src).expect("parse");
        assert_eq!(cfg.project.name.as_deref(), Some("myapp"));
        assert_eq!(cfg.commands.len(), 1);
        assert_eq!(
            cfg.commands["up"].run,
            Run::Single("docker compose up -d".into())
        );
    }

    #[test]
    fn parses_array_run() {
        let src = r#"
            [command.check]
            run = ["check:frontend", "check:backend"]
        "#;

        let cfg: Config = toml::from_str(src).unwrap();
        let cmd = &cfg.commands["check"];
        assert_eq!(cmd.run.len(), 2);
        assert!(matches!(cmd.run, Run::Steps(_)));
    }

    #[test]
    fn parses_recipe_with_full_options() {
        let src = r#"
            [command.test]
            desc = "Run test suite"
            needs = ["up"]
            in = "app"
            run = "composer test"
            forward_args = true
            tty = false
            env = { APP_ENV = "testing" }
        "#;

        let cfg: Config = toml::from_str(src).unwrap();
        let cmd = &cfg.commands["test"];
        assert_eq!(cmd.desc.as_deref(), Some("Run test suite"));
        assert_eq!(cmd.service.as_deref(), Some("app"));
        assert!(cmd.forward_args);
        assert_eq!(cmd.env.get("APP_ENV").map(String::as_str), Some("testing"));
        assert_eq!(cmd.needs, vec!["up"]);
    }

    #[test]
    fn parses_ui_panes() {
        let src = r#"
            [[ui.pane]]
            type = "service"
            service = "app"
            key = "1"

            [[ui.pane]]
            type = "watcher"
            glob = ["src/**"]
            on_change = "test"
            debounce_ms = 500
        "#;

        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.ui.panes.len(), 2);
        match &cfg.ui.panes[0] {
            UiPane::Service { service, .. } => assert_eq!(service, "app"),
            _ => panic!("expected Service pane"),
        }
        match &cfg.ui.panes[1] {
            UiPane::Watcher { debounce_ms, .. } => assert_eq!(*debounce_ms, 500),
            _ => panic!("expected Watcher pane"),
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let src = r#"
            [command.test]
            run = "x"
            unknown_field = true
        "#;

        let err = toml::from_str::<Config>(src).unwrap_err();
        assert!(err.to_string().contains("unknown_field"));
    }

    #[test]
    fn defaults_runtime_when_section_missing() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.runtime.backend, Backend::Compose);
        assert!(cfg.runtime.compose_passthrough);
        assert!(cfg.runtime.service_passthrough);
    }
}
