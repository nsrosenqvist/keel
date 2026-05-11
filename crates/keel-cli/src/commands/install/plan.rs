//! Turn `[install]` config plus `.keel/install/*` into an ordered,
//! resolved plan the runner can iterate.
//!
//! Three resolution rules:
//!
//! 1. When `[install].steps` is empty, the plan **is** the discovered
//!    step list in directory order. Authors who only use shell files
//!    don't have to touch `keel.toml` to wire them up.
//! 2. When `[install].steps` is set, every named entry must resolve
//!    against `.keel/install/`, a `[command.*]` recipe, or — by
//!    explicit opt-in — `.keel/commands/`. Unresolved names are a
//!    fatal config error.
//! 3. When `install.install_git_hooks` is true (default), a synthetic
//!    "install-hooks" step is appended. The runner knows how to fulfil
//!    it; authors don't need to provide a script.

use anyhow::{Result, bail};
use keel_config::{Config, InlineStep, InstallStepRef, InstallStepScript};
use std::path::PathBuf;

/// Final, executable step plan.
#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub desc: Option<String>,
    pub source: StepSource,
    pub interactive: bool,
    pub optional: bool,
}

impl Step {
    /// Display label for the renderer — falls back to the step name
    /// when no description is set.
    pub fn label(&self) -> &str {
        self.desc.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone)]
pub enum StepSource {
    /// Execute via [`keel_runtime::Executor::run_recipe`].
    Recipe(String),
    /// Execute a script file under `.keel/install/` directly.
    Script(InstallStepScript),
    /// Inline `run = "..."` from `[install].steps`.
    Inline(InlineStep),
    /// Built-in: install git-hook shims and prefetch any external hook
    /// repos referenced by `.pre-commit-config.yaml`.
    InstallHooks,
    /// Built-in: apply agent instructions / skills via `keel-agents`.
    /// Runs before [`StepSource::InstallHooks`] so any hook-related
    /// docs land in place first.
    ApplyAgents,
}

/// Resolve the install plan against `config`. The `project_root` is
/// accepted for parity with future resolution rules (e.g. scripts in
/// nested directories) but currently unused.
pub fn resolve(config: &Config, _project_root: &std::path::Path) -> Result<Vec<Step>> {
    let mut steps = if config.install.steps.is_empty() {
        steps_from_discovery(config)
    } else {
        steps_from_explicit(config)?
    };

    if config.agents.install_with_setup && !config.agents.sources.is_empty() {
        steps.push(Step {
            name: "apply-agents".into(),
            desc: Some("Apply agent instructions and skills from upstream sources".into()),
            source: StepSource::ApplyAgents,
            interactive: false,
            optional: false,
        });
    }

    if config.install.install_git_hooks {
        steps.push(Step {
            name: "install-hooks".into(),
            desc: Some("Install git hooks and prefetch external hook repos".into()),
            source: StepSource::InstallHooks,
            interactive: false,
            optional: false,
        });
    }

    // Reject duplicate step names — the state file uses name as the
    // identity key, so collisions would shadow each other on resume.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for step in &steps {
        if !seen.insert(step.name.as_str()) {
            bail!(
                "install plan has duplicate step name `{}`; rename one so resume can disambiguate",
                step.name
            );
        }
    }

    Ok(steps)
}

fn steps_from_discovery(config: &Config) -> Vec<Step> {
    config
        .install
        .discovered_order
        .iter()
        .filter_map(|name| {
            config
                .install
                .discovered
                .get(name)
                .map(|script| step_from_script(script.clone()))
        })
        .collect()
}

fn steps_from_explicit(config: &Config) -> Result<Vec<Step>> {
    let mut steps = Vec::with_capacity(config.install.steps.len());
    for entry in &config.install.steps {
        steps.push(resolve_entry(config, entry)?);
    }
    Ok(steps)
}

fn resolve_entry(config: &Config, entry: &InstallStepRef) -> Result<Step> {
    match entry {
        InstallStepRef::Inline(step) => Ok(step_from_inline(step.clone())),
        InstallStepRef::Name(name) => {
            if let Some(script) = config.install.discovered.get(name) {
                return Ok(step_from_script(script.clone()));
            }
            if config.commands.contains_key(name) {
                return Ok(Step {
                    name: name.clone(),
                    desc: config.commands.get(name).and_then(|r| r.desc.clone()),
                    source: StepSource::Recipe(name.clone()),
                    interactive: false,
                    optional: false,
                });
            }
            // We deliberately do *not* fall back to `.keel/commands/`
            // scripts — those are user-facing recipes that show up in
            // `keel list`, and we want install authors to be
            // explicit about wiring one into the install flow.
            bail!(
                "install step `{}` does not match a discovered .keel/install/ file \
                 or a `[command.*]` recipe. Add the file, declare the recipe, or use \
                 an inline `{{ name = \"...\", run = \"...\" }}` step.",
                name
            );
        }
    }
}

fn step_from_script(script: InstallStepScript) -> Step {
    Step {
        name: script.name.clone(),
        desc: script.desc.clone(),
        interactive: script.interactive,
        optional: script.optional,
        source: StepSource::Script(script),
    }
}

fn step_from_inline(inline: InlineStep) -> Step {
    Step {
        name: inline.name.clone(),
        desc: inline.desc.clone(),
        interactive: inline.interactive,
        optional: inline.optional,
        source: StepSource::Inline(inline),
    }
}

/// Resolve the working directory for a step. Relative paths are
/// joined onto `project_root`; absolute paths pass through.
pub fn resolve_cwd(cwd: Option<&str>, project_root: &std::path::Path) -> PathBuf {
    match cwd {
        None => project_root.to_path_buf(),
        Some(s) => {
            let p = std::path::Path::new(s);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                project_root.join(p)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_config::{Config, InstallStepScript};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn empty_script(name: &str, interactive: bool, optional: bool) -> InstallStepScript {
        InstallStepScript {
            name: name.into(),
            path: PathBuf::from(format!("/tmp/{name}")),
            desc: None,
            service: None,
            tty: false,
            env: BTreeMap::new(),
            cwd: None,
            optional,
            interactive,
        }
    }

    #[test]
    fn discovery_drives_plan_when_steps_unset() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.install.discovered_order = vec!["01-a".into(), "02-b".into()];
        cfg.install
            .discovered
            .insert("01-a".into(), empty_script("01-a", false, false));
        cfg.install
            .discovered
            .insert("02-b".into(), empty_script("02-b", true, true));

        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].name, "01-a");
        assert_eq!(plan[1].name, "02-b");
        assert!(plan[1].interactive);
        assert!(plan[1].optional);
    }

    #[test]
    fn explicit_steps_override_discovery_order() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.install
            .steps
            .push(InstallStepRef::Name("second".into()));
        cfg.install.steps.push(InstallStepRef::Name("first".into()));
        cfg.install
            .discovered
            .insert("first".into(), empty_script("first", false, false));
        cfg.install
            .discovered
            .insert("second".into(), empty_script("second", false, false));

        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert_eq!(plan[0].name, "second");
        assert_eq!(plan[1].name, "first");
    }

    #[test]
    fn unknown_step_name_is_a_config_error() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.install
            .steps
            .push(InstallStepRef::Name("missing".into()));

        let err = resolve(&cfg, std::path::Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn install_hooks_step_is_appended_by_default() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = true; // default
        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(matches!(plan[0].source, StepSource::InstallHooks));
    }

    #[test]
    fn apply_agents_step_runs_before_install_hooks() {
        use keel_config::SourceSpec;
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = true;
        cfg.agents.install_with_setup = true;
        cfg.agents.sources.push(SourceSpec {
            name: "x".into(),
            repo: "https://example.com/x.git".into(),
            rev: "v1".into(),
            subpath: None,
            manifest_path: None,
            overrides: vec![],
        });
        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert!(matches!(plan[0].source, StepSource::ApplyAgents));
        assert!(matches!(plan[1].source, StepSource::InstallHooks));
    }

    #[test]
    fn apply_agents_step_skipped_when_no_sources() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.agents.install_with_setup = true; // default
        // sources empty — nothing to apply
        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn apply_agents_step_skipped_when_install_with_setup_false() {
        use keel_config::SourceSpec;
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.agents.install_with_setup = false;
        cfg.agents.sources.push(SourceSpec {
            name: "x".into(),
            repo: "https://example.com/x.git".into(),
            rev: "v1".into(),
            subpath: None,
            manifest_path: None,
            overrides: vec![],
        });
        let plan = resolve(&cfg, std::path::Path::new("/")).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn duplicate_step_names_rejected() {
        let mut cfg = Config::default();
        cfg.install.install_git_hooks = false;
        cfg.install.steps.push(InstallStepRef::Inline(InlineStep {
            name: "x".into(),
            run: "true".into(),
            desc: None,
            service: None,
            tty: false,
            env: BTreeMap::new(),
            cwd: None,
            optional: false,
            interactive: false,
        }));
        cfg.install.steps.push(InstallStepRef::Inline(InlineStep {
            name: "x".into(),
            run: "true".into(),
            desc: None,
            service: None,
            tty: false,
            env: BTreeMap::new(),
            cwd: None,
            optional: false,
            interactive: false,
        }));

        let err = resolve(&cfg, std::path::Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("duplicate step name"));
    }
}
