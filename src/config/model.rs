//! Domain model for `keel.toml`.
//!
//! Types here are value objects: they are constructed from the TOML source,
//! validated, and then handed (immutable) to the runtime. They expose no
//! behaviour beyond accessors. Behaviour lives in `keel-runtime`.

use crate::config::agents::AgentsConfig;
use crate::config::install::InstallConfig;
use crate::config::scripts::ScriptCommand;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Top-level configuration loaded from a project's `keel.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub project: ProjectConfig,

    /// Container-runtime configuration (compose / podman / docker /
    /// none). Co-exists with `[[services.custom]]` and
    /// `[[services.systemd]]`; the registry combines them into a
    /// single backend at runtime. The TOML key is `[runtime]` — it
    /// describes the container runtime layer of the workspace, with
    /// non-container services living under `[[services.*]]`.
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

    /// Project-defined hooks. `[hooks]` in `keel.toml` is a table whose
    /// keys are stage names (`pre-commit`, `pre-push`, ...) and whose
    /// values are lists of recipe / script names to run for that stage.
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Per-worktree isolation settings. Drives `KEEL_WORKTREE_*` env
    /// var injection and the `COMPOSE_PROJECT_NAME` prefix.
    #[serde(default)]
    pub worktrees: WorktreesConfig,

    /// Diff-view settings. Lets the user pin the trunk branch the
    /// diff view uses as its merge-base anchor when auto-detection
    /// can't pick the right one.
    #[serde(default)]
    pub diff: DiffConfig,

    /// Non-container services keel tracks alongside whatever the
    /// container backend manages (compose, podman, etc.). Lets the
    /// TUI and the lifecycle keymap operate on a system Postgres,
    /// a `tunnel` daemon, etc. as if they were compose services.
    #[serde(default)]
    pub services: ServicesConfig,

    /// Scripts discovered under `.keel/commands/`. Populated by
    /// [`crate::config::loader::load_project`]; never serialized.
    #[serde(skip)]
    pub scripts: BTreeMap<String, ScriptCommand>,

    /// Install-time configuration: ordered step plan plus the steps
    /// discovered under `.keel/install/`. Authors who don't ship an
    /// install flow can ignore this entirely — `[install]` defaults to
    /// "no steps, hooks still installed automatically".
    #[serde(default)]
    pub install: InstallConfig,

    /// Agent-instruction sources: `[[agents.sources]]` entries plus the
    /// `[agents]` knobs that control them. Drives `keel agents
    /// install/update` and the synthetic install step.
    #[serde(default)]
    pub agents: AgentsConfig,

    /// Opt-in devcontainer integration. When enabled and a
    /// `.devcontainer/devcontainer.json` (or `.devcontainer.json`) is
    /// found, TUI new-shell sessions and recipes without `in = ...`
    /// route into the devcontainer instead of the host.
    #[serde(default)]
    pub devcontainer: DevcontainerConfig,
}

/// Devcontainer integration toggle.
///
/// Off by default — turning it on flips the host-execution target for
/// recipes (no `in`) and TUI terminal sessions over to a docker-managed
/// workspace container described by `devcontainer.json`. See
/// `docs/devcontainer.md` for the supported spec subset.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DevcontainerConfig {
    /// Master switch. When false, keel behaves exactly as before
    /// even if a `devcontainer.json` exists on disk.
    #[serde(default)]
    pub enabled: bool,

    /// Override the auto-detected `devcontainer.json` path
    /// (project-root-relative or absolute). When unset, keel looks
    /// for `.devcontainer/devcontainer.json` then `.devcontainer.json`.
    #[serde(default)]
    pub path: Option<String>,
}

/// Worktree isolation configuration.
///
/// keel gives each git worktree a deterministic identity (slug +
/// integer offset). Recipes reference these via the
/// `KEEL_WORKTREE_OFFSET` env var (typically through the
/// [`EnvSpec::base`] / [`EnvSpec::offset`] arithmetic shorthand) so two
/// worktrees of the same project can run side-by-side without port
/// collisions.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreesConfig {
    /// Range cap for hash-based offsets: `offset = hash(seed + slug) %
    /// modulus`. Default 1000 — keeps offset additions to ports inside
    /// a sane range while making collisions rare even with 10+
    /// worktrees (~5%).
    #[serde(default = "default_modulus")]
    pub modulus: u32,

    /// Extra hash seed appended to the slug before hashing. Defaults to
    /// the project name (set in [`Config::resolved_seed`]). Letting
    /// users override it gives them a way to bump every worktree's
    /// offset at once if they need to dodge a port range.
    #[serde(default)]
    pub seed: String,

    /// When true (default) and a worktree slug is non-empty, keel
    /// sets `COMPOSE_PROJECT_NAME = <project>-<slug>` so each
    /// worktree's docker compose stack is independent. Skipped when the
    /// user already declared `COMPOSE_PROJECT_NAME` themselves.
    #[serde(default = "true_default")]
    pub isolate_compose: bool,

    /// Explicit slug → offset pins. Wins over the hash. Use for slugs
    /// that should always have a known offset (e.g. `main = 0`,
    /// `production = 0`).
    #[serde(default)]
    pub assign: BTreeMap<String, u32>,

    /// When set, keel materialises the resolved `[env]` (plus the
    /// three worktree-derived built-ins) into this dotenv file as a
    /// marker-delimited block on every CLI invocation, and `keel
    /// hooks install` (without explicit `--stages`) auto-includes
    /// `post-checkout` / `post-merge` so the file stays fresh even
    /// when the user runs `docker compose` directly. Path is
    /// project-root-relative unless absolute. Omitting the field
    /// preserves the original opt-in behaviour: nothing is written
    /// until the user runs `keel env --write` themselves.
    #[serde(default)]
    pub dotenv: Option<String>,
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        Self {
            modulus: default_modulus(),
            seed: String::new(),
            isolate_compose: true,
            assign: BTreeMap::new(),
            dotenv: None,
        }
    }
}

const fn default_modulus() -> u32 {
    1000
}

impl Config {
    /// Resolved hash seed for worktree offset computation: the
    /// `[worktrees].seed` value if set, otherwise the project name (so
    /// two projects with `feature-x` get different offsets).
    pub fn resolved_seed(&self) -> &str {
        if !self.worktrees.seed.is_empty() {
            return &self.worktrees.seed;
        }
        self.project.name.as_deref().unwrap_or("")
    }

    /// Structural validation. Catches issues that pure deserialization
    /// can't (cross-field invariants, name collisions). Backend-aware
    /// checks (e.g. "this name also exists in compose") happen later
    /// against a live backend; this is the I/O-free layer.
    pub fn validate(&self) -> Result<(), String> {
        // Service-name uniqueness across `services.custom` and
        // `services.systemd`. Compose-discovered names join the union
        // at runtime (the registry handles that collision separately).
        let mut seen: std::collections::HashMap<&str, &'static str> =
            std::collections::HashMap::new();
        for svc in &self.services.custom {
            if let Some(prev) = seen.insert(svc.name.as_str(), "services.custom") {
                return Err(format!(
                    "duplicate service name `{}` (previously declared in {prev})",
                    svc.name
                ));
            }
        }
        for svc in &self.services.systemd {
            if let Some(prev) = seen.insert(svc.name.as_str(), "services.systemd") {
                return Err(format!(
                    "duplicate service name `{}` (previously declared in {prev})",
                    svc.name
                ));
            }
        }
        self.agents.validate()?;
        Ok(())
    }
}

impl CustomService {
    /// Resolve the four lifecycle commands plus the optional log
    /// command. `restart` falls back to `<stop> && <start>` when the
    /// user didn't supply one explicitly. Returned by reference to
    /// avoid allocating per-call; the synthesised restart is held in
    /// a `Cow` so the caller doesn't have to care.
    pub fn restart_cmd(&self) -> std::borrow::Cow<'_, str> {
        match &self.restart {
            Some(r) => std::borrow::Cow::Borrowed(r.as_str()),
            None => std::borrow::Cow::Owned(format!("{} && {}", self.stop, self.start)),
        }
    }
}

/// Non-container services declared in `[[services.custom]]` and
/// `[[services.systemd]]`. Both feed a single `CustomBackend` at
/// runtime; `systemd` is just schema sugar (a unit name + scope
/// expand to start/stop/restart/status/logs commands).
///
/// Both arrays default to empty — a project that only uses compose
/// pays nothing for this.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServicesConfig {
    #[serde(default)]
    pub custom: Vec<CustomService>,
    #[serde(default)]
    pub systemd: Vec<SystemdService>,
}

/// Generic service: the user supplies the shell commands keel runs
/// for each lifecycle action. `status` is required because the TUI
/// needs to render running/stopped state; `start` and `stop` are
/// required because every keymap action needs a target. The rest are
/// optional with sensible fallbacks (see [`CustomService::actions`]).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CustomService {
    pub name: String,
    #[serde(default)]
    pub desc: Option<String>,
    /// Exit code 0 = `Running`; non-zero = `Stopped`. Stdout / stderr
    /// are not parsed — exit code is the contract.
    pub status: String,
    pub start: String,
    pub stop: String,
    /// Defaults to `<stop> && <start>` when absent.
    #[serde(default)]
    pub restart: Option<String>,
    /// Long-running command whose stdout / stderr is streamed into
    /// the TUI service pane. Absent → the pane shows "no log source".
    #[serde(default)]
    pub logs: Option<String>,
}

/// Systemd-controlled service: schema sugar that compiles down to a
/// `CustomService` at backend-construction time. We keep the surface
/// minimal — anything more elaborate (custom systemctl flags, drop-in
/// envs, etc.) belongs in `services.custom`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SystemdService {
    pub name: String,
    #[serde(default)]
    pub desc: Option<String>,
    /// The systemd unit name, e.g. `postgresql.service`. The `.service`
    /// suffix is conventional but optional — systemctl accepts both.
    pub unit: String,
    #[serde(default)]
    pub scope: SystemdScope,
}

/// Whether a systemd service runs in the user or system instance.
/// Defaults to `User` because dev-loop services are almost always
/// per-user — a shared Postgres on `system` is the exception.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SystemdScope {
    #[default]
    User,
    System,
}

/// Native keel hook configuration. Keyed by stage name.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct HooksConfig {
    pub stages: BTreeMap<String, Vec<String>>,
}

impl HooksConfig {
    pub fn for_stage(&self, stage: &str) -> &[String] {
        self.stages
            .get(stage)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    pub fn declared_stages(&self) -> impl Iterator<Item = &str> {
        self.stages.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Diff-view configuration.
///
/// The diff view scopes its file list and per-file diffs against a
/// merge-base with the project's trunk branch — i.e. "everything
/// I've changed since I diverged from main." When `base` is unset,
/// keel picks a trunk via:
///   1. `git symbolic-ref refs/remotes/origin/HEAD` (the remote
///      default branch — the canonical answer).
///   2. Local fallback: `main`, `master`, `develop`, `trunk` (in
///      that order, first match wins).
///   3. If none exist: fall back to `git diff HEAD` (current
///      working-tree-vs-last-commit behaviour).
///
/// Set `base` to override the trunk explicitly. Useful for projects
/// that don't follow a conventional trunk name (e.g. `release/stable`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffConfig {
    /// Branch name to use as the diff base. None → auto-detect.
    #[serde(default)]
    pub base: Option<String>,
}

/// Container-runtime configuration: which backend to talk to
/// (compose / podman / docker / none) plus passthrough toggles.
/// The TOML key is `[runtime]`; non-container services live under
/// `[[services.*]]`, so this section describes the workspace's
/// container runtime specifically — the layer that orchestrates
/// containers, whether that's compose, podman, or (in the future)
/// minikube.
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
/// Resolution order (first match wins):
///
/// 1. Explicit `value`.
/// 2. `base` + `offset`: integer-typed shorthand for
///    `value = base.parse::<i64>() + existing[offset].parse::<i64>()`.
///    The `offset` lookup is on the env-so-far, so referencing
///    `KEEL_WORKTREE_OFFSET` makes ports automatically vary per
///    worktree. Missing offset var → falls back to `base`.
/// 3. Pre-existing process / dotenv value for the same name.
/// 4. `from_command` stdout (trimmed).
/// 5. `default`.
///
/// `required = true` with no resolved value produces
/// `RuntimeError::RequiredEnvMissing`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
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

    /// Integer base for `base + offset` arithmetic. Stored as a string
    /// so users can write `"8080"` consistently with the rest of the
    /// schema; parsed as `i64` at resolution time.
    #[serde(default)]
    pub base: Option<String>,

    /// Name of the env var whose value is parsed as an integer offset
    /// added to `base`. Typically `"KEEL_WORKTREE_OFFSET"`.
    #[serde(default)]
    pub offset: Option<String>,
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
    /// Multi-step logic beyond a flat list belongs in `.keel/commands/`.
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

    /// Named overrides activated by `--profile <name>`. Each profile may
    /// override a subset of the recipe's fields.
    #[serde(default)]
    pub profile: BTreeMap<String, RecipeProfile>,
}

/// Override layer applied on top of a [`Recipe`] when a named profile is
/// active. Every field is optional; missing fields leave the recipe's
/// value untouched.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecipeProfile {
    #[serde(default)]
    pub run: Option<Run>,

    #[serde(default, rename = "in")]
    pub service: Option<String>,

    #[serde(default)]
    pub tty: Option<bool>,

    /// Env entries from the profile are merged on top of the recipe's
    /// `env`. Profile keys win.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    pub needs: Option<Vec<String>>,

    #[serde(default)]
    pub forward_args: Option<bool>,

    #[serde(default)]
    pub parallel: Option<bool>,
}

impl Recipe {
    /// Apply the named profile (if it exists) and return the effective
    /// recipe. The original is left untouched.
    pub fn with_profile(&self, profile_name: Option<&str>) -> std::borrow::Cow<'_, Recipe> {
        let Some(name) = profile_name else {
            return std::borrow::Cow::Borrowed(self);
        };
        let Some(profile) = self.profile.get(name) else {
            return std::borrow::Cow::Borrowed(self);
        };
        let mut merged = self.clone();
        if let Some(run) = &profile.run {
            merged.run = run.clone();
        }
        if let Some(service) = &profile.service {
            merged.service = Some(service.clone());
        }
        if let Some(tty) = profile.tty {
            merged.tty = tty;
        }
        for (k, v) in &profile.env {
            merged.env.insert(k.clone(), v.clone());
        }
        if let Some(needs) = &profile.needs {
            merged.needs = needs.clone();
        }
        if let Some(forward_args) = profile.forward_args {
            merged.forward_args = forward_args;
        }
        if let Some(parallel) = profile.parallel {
            merged.parallel = parallel;
        }
        std::borrow::Cow::Owned(merged)
    }
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
    fn worktrees_defaults_when_section_absent() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.worktrees.modulus, 1000);
        assert!(cfg.worktrees.seed.is_empty());
        assert!(cfg.worktrees.isolate_compose);
        assert!(cfg.worktrees.assign.is_empty());
        assert!(cfg.worktrees.dotenv.is_none());
    }

    #[test]
    fn worktrees_parses_full_section() {
        let src = r#"
            [worktrees]
            modulus = 100
            seed = "myseed"
            isolate_compose = false
            dotenv = ".env"

            [worktrees.assign]
            main = 0
            production = 0
            "feature/x" = 7
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.worktrees.modulus, 100);
        assert_eq!(cfg.worktrees.seed, "myseed");
        assert!(!cfg.worktrees.isolate_compose);
        assert_eq!(cfg.worktrees.dotenv.as_deref(), Some(".env"));
        assert_eq!(cfg.worktrees.assign.get("main"), Some(&0));
        assert_eq!(cfg.worktrees.assign.get("feature/x"), Some(&7));
    }

    #[test]
    fn resolved_seed_falls_back_to_project_name() {
        let cfg: Config = toml::from_str(
            r#"[project]
            name = "myapp"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_seed(), "myapp");
    }

    #[test]
    fn resolved_seed_prefers_explicit_seed() {
        let cfg: Config = toml::from_str(
            r#"
            [project]
            name = "myapp"

            [worktrees]
            seed = "custom"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_seed(), "custom");
    }

    #[test]
    fn env_spec_parses_base_offset() {
        let cfg: Config = toml::from_str(
            r#"
            [env]
            APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
        "#,
        )
        .unwrap();
        let spec = &cfg.env["APP_PORT"];
        assert_eq!(spec.base.as_deref(), Some("8080"));
        assert_eq!(spec.offset.as_deref(), Some("KEEL_WORKTREE_OFFSET"));
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
    fn parses_recipe_profiles() {
        let src = r#"
            [command.test]
            run = "composer test"

            [command.test.profile.ci]
            tty = false
            forward_args = true

            [command.test.profile.ci.env]
            XDEBUG_MODE = "off"

            [command.test.profile.parallel]
            parallel = true
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let test = &cfg.commands["test"];
        assert_eq!(test.profile.len(), 2);
        let ci = &test.profile["ci"];
        assert_eq!(ci.tty, Some(false));
        assert_eq!(ci.forward_args, Some(true));
        assert_eq!(ci.env.get("XDEBUG_MODE").map(String::as_str), Some("off"));
    }

    #[test]
    fn with_profile_applies_overrides() {
        let cfg: Config = toml::from_str(
            r#"
                [command.test]
                run = "composer test"
                tty = true

                [command.test.profile.ci]
                tty = false
                env = { XDEBUG_MODE = "off" }
            "#,
        )
        .unwrap();
        let test = &cfg.commands["test"];
        let active = test.with_profile(Some("ci"));
        assert!(!active.tty);
        assert_eq!(
            active.env.get("XDEBUG_MODE").map(String::as_str),
            Some("off")
        );
        // No profile name → identity.
        let untouched = test.with_profile(None);
        assert!(untouched.tty);
    }

    #[test]
    fn with_profile_unknown_returns_original() {
        let cfg: Config = toml::from_str(
            r#"
                [command.test]
                run = "true"
            "#,
        )
        .unwrap();
        let active = cfg.commands["test"].with_profile(Some("does-not-exist"));
        assert_eq!(*active, cfg.commands["test"]);
    }

    #[test]
    fn parses_hooks_config() {
        let src = r#"
            [hooks]
            pre-commit = ["check:format", "check:lint"]
            pre-push = ["test"]
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(
            cfg.hooks.for_stage("pre-commit"),
            &["check:format", "check:lint"]
        );
        assert_eq!(cfg.hooks.for_stage("pre-push"), &["test"]);
        assert!(cfg.hooks.for_stage("post-commit").is_empty());
    }

    #[test]
    fn services_default_to_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.services.custom.is_empty());
        assert!(cfg.services.systemd.is_empty());
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_custom_service_minimal() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.custom]]
            name   = "ngrok"
            status = "pgrep -x ngrok"
            start  = "ngrok http 8080"
            stop   = "pkill -x ngrok"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.services.custom.len(), 1);
        let svc = &cfg.services.custom[0];
        assert_eq!(svc.name, "ngrok");
        assert_eq!(svc.status, "pgrep -x ngrok");
        assert!(svc.restart.is_none());
        assert!(svc.logs.is_none());
        // Synthesised restart command falls back to stop && start.
        assert_eq!(svc.restart_cmd(), "pkill -x ngrok && ngrok http 8080");
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_custom_service_with_restart_and_logs() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.custom]]
            name    = "tunnel"
            status  = "true"
            start   = "true"
            stop    = "true"
            restart = "kill -HUP $(pgrep tunnel)"
            logs    = "tail -f /tmp/tunnel.log"
        "#,
        )
        .unwrap();
        let svc = &cfg.services.custom[0];
        assert_eq!(svc.restart_cmd(), "kill -HUP $(pgrep tunnel)");
        assert_eq!(svc.logs.as_deref(), Some("tail -f /tmp/tunnel.log"));
    }

    #[test]
    fn parses_systemd_service_with_default_scope() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.systemd]]
            name = "postgres"
            unit = "postgresql.service"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.services.systemd.len(), 1);
        let svc = &cfg.services.systemd[0];
        assert_eq!(svc.name, "postgres");
        assert_eq!(svc.unit, "postgresql.service");
        assert_eq!(svc.scope, SystemdScope::User);
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_systemd_service_with_explicit_system_scope() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.systemd]]
            name  = "shared-db"
            unit  = "postgresql.service"
            scope = "system"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.services.systemd[0].scope, SystemdScope::System);
    }

    #[test]
    fn validate_rejects_duplicate_custom_names() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.custom]]
            name = "x"
            status = "true"
            start  = "true"
            stop   = "true"

            [[services.custom]]
            name = "x"
            status = "true"
            start  = "true"
            stop   = "true"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("duplicate service name `x`"));
        assert!(err.contains("services.custom"));
    }

    #[test]
    fn validate_rejects_name_collision_across_custom_and_systemd() {
        let cfg: Config = toml::from_str(
            r#"
            [[services.custom]]
            name = "postgres"
            status = "true"
            start  = "true"
            stop   = "true"

            [[services.systemd]]
            name = "postgres"
            unit = "postgresql.service"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("duplicate service name `postgres`"));
        assert!(err.contains("services.custom"));
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
    fn devcontainer_defaults_when_section_absent() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.devcontainer.enabled);
        assert!(cfg.devcontainer.path.is_none());
    }

    #[test]
    fn parses_devcontainer_section() {
        let src = r#"
            [devcontainer]
            enabled = true
            path = ".devcontainer/devcontainer.json"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(cfg.devcontainer.enabled);
        assert_eq!(
            cfg.devcontainer.path.as_deref(),
            Some(".devcontainer/devcontainer.json")
        );
    }

    #[test]
    fn devcontainer_rejects_unknown_fields() {
        let src = r#"
            [devcontainer]
            enabled = true
            unexpected = "nope"
        "#;
        let err = toml::from_str::<Config>(src).unwrap_err();
        assert!(err.to_string().contains("unexpected"));
    }

    #[test]
    fn defaults_runtime_when_section_missing() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.runtime.backend, Backend::Compose);
        assert!(cfg.runtime.compose_passthrough);
        assert!(cfg.runtime.service_passthrough);
    }
}
