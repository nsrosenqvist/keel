//! Environment variable resolution for recipe execution.
//!
//! Merge order, from least to most specific (later wins):
//!
//! 1. Inherited process env.
//! 2. `.env` files declared in `[env_files]`, in order. `${VAR}` in path
//!    strings is expanded against the env so far.
//! 3. Project `[env]` specs, resolved as
//!    `value` → existing env → `from_command` → `default`. `required = true`
//!    with no resolved value produces a [`RuntimeError::RequiredEnvMissing`].
//! 4. Per-recipe `env = {...}` overrides applied at run-step time.
//!
//! Resolution is async because `from_command` spawns a process. The
//! [`Executor`](crate::runtime::Executor) caches the result in a [`tokio::sync::OnceCell`]
//! so multi-step or dependency-chained recipes pay the cost once per
//! invocation.

use crate::config::{Config, EnvSpec};
use crate::runtime::error::RuntimeError;
use crate::runtime::worktree::Identity;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tokio::process::Command;

/// Resolved environment ready to hand to a process.
///
/// Two views live side by side:
///   * `vars` — the merged result of process env + project config; used
///     for host-side execution where the child should inherit everything
///     a normal shell would (PATH, HOME, …) plus project overrides.
///   * `project_keys` — the subset of keys *declared* by project config
///     (dotenv files, KEEL_* injection, `[env]`, recipe / script
///     overrides). Used for container exec, where leaking host PATH
///     etc. via `-e KEY=VAL` would overwrite the image's defaults and
///     break command resolution.
#[derive(Debug, Default, Clone)]
pub struct Env {
    vars: BTreeMap<String, String>,
    project_keys: BTreeSet<String>,
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a fresh env from the inherited process env without applying
    /// any project config. Useful for tests and as a base for manual builds.
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
            project_keys: BTreeSet::new(),
        }
    }

    /// Resolve the project base env: process + dotenv + worktree
    /// injection + `[env]` section.
    ///
    /// Auto-detects the worktree identity by shelling out to `git`. For
    /// callers that already have an [`Identity`] (CLI, TUI), prefer
    /// [`Self::resolve_with_identity`] to avoid duplicate detection.
    pub async fn resolve(config: &Config, project_root: &Path) -> Result<Self, RuntimeError> {
        let identity = Identity::detect(project_root, config).await;
        Self::resolve_with_identity(config, project_root, &identity).await
    }

    /// Resolve the project base env using a previously-detected identity.
    pub async fn resolve_with_identity(
        config: &Config,
        project_root: &Path,
        identity: &Identity,
    ) -> Result<Self, RuntimeError> {
        let mut vars: BTreeMap<String, String> = std::env::vars().collect();
        let mut project_keys: BTreeSet<String> = BTreeSet::new();

        for raw_path in &config.env_files.files {
            let expanded = expand_vars(raw_path, &vars);
            let path = if Path::new(&expanded).is_absolute() {
                std::path::PathBuf::from(expanded)
            } else {
                project_root.join(&expanded)
            };
            if !path.exists() {
                continue;
            }
            for entry in
                dotenvy::from_path_iter(&path).map_err(|source| RuntimeError::DotenvIo {
                    path: path.clone(),
                    source: into_io_error(source),
                })?
            {
                let (k, v) = entry.map_err(|source| RuntimeError::DotenvParse {
                    path: path.clone(),
                    source,
                })?;
                project_keys.insert(k.clone());
                vars.insert(k, v);
            }
        }

        // Inject worktree-derived env *before* [env] resolution so user
        // entries can reference KEEL_WORKTREE_OFFSET via `offset`.
        inject_worktree_env(&mut vars, &mut project_keys, config, identity);

        for (name, spec) in &config.env {
            if let Some(value) = resolve_spec(name, spec, &vars).await? {
                project_keys.insert(name.clone());
                vars.insert(name.clone(), value);
            }
        }

        Ok(Self { vars, project_keys })
    }

    /// Apply per-recipe overrides on top of the base env. Overrides are
    /// always project-declared (they came from `[command.X.env]`, a
    /// script's `env = {...}`, or KEEL_*-prefixed executor injection) —
    /// mark them so they survive the project-only filter used for
    /// container exec.
    pub fn with_overrides<I, K, V>(mut self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in overrides {
            let k = k.into();
            self.project_keys.insert(k.clone());
            self.vars.insert(k, v.into());
        }
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn into_map(self) -> BTreeMap<String, String> {
        self.vars
    }

    /// Snapshot of the project-declared subset only. Use this for
    /// container exec, where inherited host vars (PATH, HOME, …)
    /// would otherwise overwrite the image's defaults and break
    /// command resolution. Keys come from dotenv files, KEEL_*
    /// injection, `[env]`, and recipe / script overrides.
    pub fn project_only_map(&self) -> BTreeMap<String, String> {
        self.project_keys
            .iter()
            .filter_map(|k| self.vars.get(k).map(|v| (k.clone(), v.clone())))
            .collect()
    }

    /// Apply this env to `cmd`, replacing the inherited process
    /// environment with exactly the resolved set. Single source of
    /// truth for the rule "keel-spawned host processes see exactly
    /// the resolved env, never the parent shell's leakage" — every
    /// host-side spawn in `Executor` goes through here.
    pub fn apply_to(&self, cmd: &mut Command) {
        cmd.env_clear();
        for (k, v) in self.iter() {
            cmd.env(k, v);
        }
    }

    /// Number of resolved variables.
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

/// Inject `KEEL_WORKTREE_*` and (when isolation is on)
/// `COMPOSE_PROJECT_NAME` into `vars`. Won't clobber values the user
/// has already set in `.env` or process env — those take priority.
/// Keys are always recorded as project-declared (even when the value
/// was kept from existing env) because keel owns the semantics of
/// these names.
fn inject_worktree_env(
    vars: &mut BTreeMap<String, String>,
    project_keys: &mut BTreeSet<String>,
    config: &Config,
    identity: &Identity,
) {
    // Always inject the slug + offset so the user can reference them
    // unconditionally in `[env]` specs. Empty slug → empty string and
    // offset 0.
    vars.entry("KEEL_WORKTREE_SLUG".into())
        .or_insert_with(|| identity.slug.clone());
    project_keys.insert("KEEL_WORKTREE_SLUG".into());
    vars.entry("KEEL_WORKTREE_OFFSET".into())
        .or_insert_with(|| identity.offset.to_string());
    project_keys.insert("KEEL_WORKTREE_OFFSET".into());

    // Compose project name: only when isolation is on, slug is
    // non-empty, and the user hasn't already set it.
    if config.worktrees.isolate_compose && identity.is_isolated() {
        if !vars.contains_key("COMPOSE_PROJECT_NAME") {
            let project = config.project.name.as_deref().unwrap_or("keel");
            vars.insert(
                "COMPOSE_PROJECT_NAME".into(),
                format!("{project}-{slug}", slug = identity.slug),
            );
        }
        project_keys.insert("COMPOSE_PROJECT_NAME".into());
    }
}

/// Resolve `base + offset` arithmetic. Returns `None` if `base` is
/// absent. Errors only when `base` is set but doesn't parse as an
/// integer; missing offset env defaults to 0.
fn resolve_base_offset(
    name: &str,
    spec: &EnvSpec,
    existing: &BTreeMap<String, String>,
) -> Result<Option<String>, RuntimeError> {
    let Some(base) = spec.base.as_ref() else {
        return Ok(None);
    };
    let base_n: i64 = base.parse().map_err(|_| RuntimeError::ArgvParse {
        input: base.clone(),
        message: format!("env `{name}`: `base` must be an integer (got `{base}`)"),
    })?;
    let offset_n: i64 = if let Some(offset_var) = spec.offset.as_ref() {
        existing
            .get(offset_var)
            .map(|s| s.parse::<i64>().unwrap_or(0))
            .unwrap_or(0)
    } else {
        0
    };
    Ok(Some((base_n + offset_n).to_string()))
}

async fn resolve_spec(
    name: &str,
    spec: &EnvSpec,
    existing: &BTreeMap<String, String>,
) -> Result<Option<String>, RuntimeError> {
    if let Some(value) = &spec.value {
        return Ok(Some(value.clone()));
    }
    if let Some(value) = resolve_base_offset(name, spec, existing)? {
        return Ok(Some(value));
    }
    if let Some(found) = existing.get(name) {
        return Ok(Some(found.clone()));
    }
    if let Some(command) = &spec.from_command {
        let argv = shell_words::split(command).map_err(|e| RuntimeError::ArgvParse {
            input: command.clone(),
            message: e.to_string(),
        })?;
        let (program, rest) = argv.split_first().ok_or_else(|| RuntimeError::ArgvParse {
            input: command.clone(),
            message: "empty command".into(),
        })?;
        let output = Command::new(program)
            .args(rest)
            .output()
            .await
            .map_err(|_| RuntimeError::EnvCommandFailed {
                var: name.into(),
                command: command.clone(),
                exit_code: None,
            })?;
        if !output.status.success() {
            return Err(RuntimeError::EnvCommandFailed {
                var: name.into(),
                command: command.clone(),
                exit_code: output.status.code(),
            });
        }
        let stdout = String::from_utf8(output.stdout).map_err(|_| RuntimeError::ArgvParse {
            input: command.clone(),
            message: "from_command output is not valid UTF-8".into(),
        })?;
        return Ok(Some(stdout.trim_end_matches(['\r', '\n']).to_string()));
    }
    if let Some(default) = &spec.default {
        return Ok(Some(default.clone()));
    }
    if spec.required {
        return Err(RuntimeError::RequiredEnvMissing(name.into()));
    }
    Ok(None)
}

/// Expand `${VAR}` references in `s` using `vars`. Unset variables expand
/// to the empty string. `$$` escapes to a literal `$`.
pub fn expand_vars(s: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '$' && i + 1 < bytes.len() {
            let next = bytes[i + 1] as char;
            if next == '$' {
                out.push('$');
                i += 2;
                continue;
            }
            if next == '{'
                && let Some(close) = bytes[i + 2..].iter().position(|b| *b == b'}')
            {
                let name = &s[i + 2..i + 2 + close];
                if let Some(v) = vars.get(name) {
                    out.push_str(v);
                }
                i += 2 + close + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Helper: extract the io::Error from a dotenvy::Error, fabricating one if
/// the underlying cause was not I/O. Used to surface a clean DotenvIo error
/// for missing files vs. an opaque dotenvy::Error wrapping in the same case.
fn into_io_error(err: dotenvy::Error) -> std::io::Error {
    match err {
        dotenvy::Error::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, EnvSpec};
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn cfg_with_env(specs: &[(&str, EnvSpec)]) -> Config {
        let mut c = Config::default();
        for (k, v) in specs {
            c.env.insert((*k).to_string(), v.clone());
        }
        c
    }

    fn write(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[tokio::test]
    async fn dotenv_loads_from_project_root() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".env", "FOO=bar\nBAZ=qux\n");
        let mut cfg = Config::default();
        cfg.env_files.files.push(".env".into());
        let env = Env::resolve(&cfg, dir.path()).await.unwrap();
        assert_eq!(env.get("FOO"), Some("bar"));
        assert_eq!(env.get("BAZ"), Some("qux"));
    }

    #[tokio::test]
    async fn later_dotenv_overrides_earlier() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".env", "APP_ENV=development\n");
        write(&dir, ".env.testing", "APP_ENV=testing\n");
        let mut cfg = Config::default();
        cfg.env_files.files.push(".env".into());
        cfg.env_files.files.push(".env.testing".into());
        let env = Env::resolve(&cfg, dir.path()).await.unwrap();
        assert_eq!(env.get("APP_ENV"), Some("testing"));
    }

    #[tokio::test]
    async fn dotenv_path_expands_vars() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".env.production", "TIER=prod\n");
        // SAFETY: tests in this file are not run concurrently with other env mutators.
        unsafe {
            std::env::set_var("APP_ENV", "production");
        }
        let mut cfg = Config::default();
        cfg.env_files.files.push(".env.${APP_ENV}".into());
        let env = Env::resolve(&cfg, dir.path()).await.unwrap();
        assert_eq!(env.get("TIER"), Some("prod"));
        unsafe {
            std::env::remove_var("APP_ENV");
        }
    }

    #[tokio::test]
    async fn missing_dotenv_files_are_silently_skipped() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.env_files.files.push(".env.does-not-exist".into());
        // The point of this test is that resolving does not error.
        let _env = Env::resolve(&cfg, dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn from_command_resolves_value() {
        let cfg = cfg_with_env(&[(
            "USER_ID",
            EnvSpec {
                value: None,
                default: None,
                from_command: Some("printf 1234".into()),
                required: false,
                ..Default::default()
            },
        )]);
        let env = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap();
        assert_eq!(env.get("USER_ID"), Some("1234"));
    }

    #[tokio::test]
    async fn from_command_failure_propagates() {
        let cfg = cfg_with_env(&[(
            "USER_ID",
            EnvSpec {
                value: None,
                default: None,
                from_command: Some("false".into()),
                required: false,
                ..Default::default()
            },
        )]);
        let err = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap_err();
        match err {
            RuntimeError::EnvCommandFailed { var, .. } => assert_eq!(var, "USER_ID"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn required_missing_errors() {
        let cfg = cfg_with_env(&[(
            "KEEL_TEST_REQ_MISSING",
            EnvSpec {
                value: None,
                default: None,
                from_command: None,
                required: true,
                ..Default::default()
            },
        )]);
        unsafe {
            std::env::remove_var("KEEL_TEST_REQ_MISSING");
        }
        let err = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap_err();
        match err {
            RuntimeError::RequiredEnvMissing(name) => {
                assert_eq!(name, "KEEL_TEST_REQ_MISSING")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn explicit_value_wins_over_existing() {
        unsafe {
            std::env::set_var("KEEL_TEST_VAR", "from-process");
        }
        let cfg = cfg_with_env(&[(
            "KEEL_TEST_VAR",
            EnvSpec {
                value: Some("from-config".into()),
                default: None,
                from_command: None,
                required: false,
                ..Default::default()
            },
        )]);
        let env = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap();
        assert_eq!(env.get("KEEL_TEST_VAR"), Some("from-config"));
        unsafe {
            std::env::remove_var("KEEL_TEST_VAR");
        }
    }

    #[tokio::test]
    async fn apply_to_clears_then_injects() {
        // Set a marker var in the parent the child should NOT see.
        // SAFETY: env mutation is fine here — these tests run in-process
        // sequentially via tokio's single-thread test runtime.
        unsafe {
            std::env::set_var("KEEL_APPLY_TO_MARKER", "leaked");
        }
        let env = Env::new().with_overrides([("FOO", "bar"), ("BAZ", "qux")]);

        let mut cmd = Command::new("sh");
        // -i prints empty for unset; we look for the literal "<unset>".
        cmd.arg("-c")
            .arg(r#"echo FOO=$FOO; echo BAZ=$BAZ; echo MARKER=${KEEL_APPLY_TO_MARKER:-<unset>}"#);
        env.apply_to(&mut cmd);

        let out = cmd.output().await.unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        unsafe {
            std::env::remove_var("KEEL_APPLY_TO_MARKER");
        }

        assert!(text.contains("FOO=bar"), "FOO injected: {text}");
        assert!(text.contains("BAZ=qux"), "BAZ injected: {text}");
        assert!(
            text.contains("MARKER=<unset>"),
            "parent env stripped: {text}"
        );
    }

    #[tokio::test]
    async fn project_only_map_excludes_inherited_host_env() {
        // SAFETY: tests in this module run sequentially.
        unsafe {
            std::env::set_var("KEEL_HOST_LEAK", "from-shell");
        }
        let cfg = cfg_with_env(&[(
            "KEEL_PROJECT_VAR",
            EnvSpec {
                value: Some("from-config".into()),
                ..Default::default()
            },
        )]);
        let env = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("KEEL_HOST_LEAK");
        }
        let project = env.project_only_map();
        assert_eq!(
            project.get("KEEL_PROJECT_VAR").map(String::as_str),
            Some("from-config")
        );
        assert!(
            !project.contains_key("KEEL_HOST_LEAK"),
            "host-only var must not leak into container env"
        );
        // PATH is the canonical host-only leak — confirm it's filtered.
        assert!(
            !project.contains_key("PATH"),
            "PATH must not be passed to container exec; it would override the image's default and break command lookup"
        );
        // KEEL_WORKTREE_* are project-declared even though their value
        // came from the executor's identity injection.
        assert!(project.contains_key("KEEL_WORKTREE_SLUG"));
        assert!(project.contains_key("KEEL_WORKTREE_OFFSET"));
    }

    #[tokio::test]
    async fn project_only_map_includes_recipe_overrides() {
        let env = Env::new().with_overrides([("MY_VAR", "from-recipe")]);
        let project = env.project_only_map();
        assert_eq!(
            project.get("MY_VAR").map(String::as_str),
            Some("from-recipe")
        );
    }

    #[tokio::test]
    async fn project_only_map_includes_dotenv_keys() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".env", "FROM_DOTENV=yes\n");
        let mut cfg = Config::default();
        cfg.env_files.files.push(".env".into());
        let env = Env::resolve(&cfg, dir.path()).await.unwrap();
        let project = env.project_only_map();
        assert_eq!(project.get("FROM_DOTENV").map(String::as_str), Some("yes"));
    }

    #[tokio::test]
    async fn recipe_overrides_win() {
        let cfg = cfg_with_env(&[(
            "APP_ENV",
            EnvSpec {
                value: Some("development".into()),
                default: None,
                from_command: None,
                required: false,
                ..Default::default()
            },
        )]);
        let env = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap()
            .with_overrides([("APP_ENV", "testing")]);
        assert_eq!(env.get("APP_ENV"), Some("testing"));
    }

    #[test]
    fn expand_vars_replaces_known() {
        let mut vars = BTreeMap::new();
        vars.insert("APP_ENV".into(), "production".into());
        assert_eq!(expand_vars(".env.${APP_ENV}", &vars), ".env.production");
    }

    #[test]
    fn expand_vars_drops_unknown() {
        let vars = BTreeMap::new();
        assert_eq!(expand_vars(".env.${MISSING}", &vars), ".env.");
    }

    #[test]
    fn expand_vars_dollar_dollar_escapes() {
        let vars = BTreeMap::new();
        assert_eq!(expand_vars("$$VAR", &vars), "$VAR");
    }

    #[test]
    fn expand_vars_handles_unterminated() {
        let vars = BTreeMap::new();
        assert_eq!(expand_vars("${UNCLOSED", &vars), "${UNCLOSED");
    }

    #[test]
    fn inject_worktree_env_sets_slug_and_offset() {
        use crate::runtime::worktree::{BaseRef, Identity};

        let mut cfg = crate::config::Config::default();
        cfg.project.name = Some("myapp".into());
        let identity = Identity {
            slug: "feature-x".into(),
            base_ref: BaseRef::Branch("feature/x".into()),
            offset: 42,
        };
        let mut vars = BTreeMap::new();
        let mut keys = BTreeSet::new();
        inject_worktree_env(&mut vars, &mut keys, &cfg, &identity);
        assert_eq!(
            vars.get("KEEL_WORKTREE_SLUG").map(String::as_str),
            Some("feature-x")
        );
        assert_eq!(
            vars.get("KEEL_WORKTREE_OFFSET").map(String::as_str),
            Some("42")
        );
        assert_eq!(
            vars.get("COMPOSE_PROJECT_NAME").map(String::as_str),
            Some("myapp-feature-x")
        );
        assert!(keys.contains("KEEL_WORKTREE_SLUG"));
        assert!(keys.contains("KEEL_WORKTREE_OFFSET"));
        assert!(keys.contains("COMPOSE_PROJECT_NAME"));
    }

    #[test]
    fn inject_worktree_env_keeps_existing_compose_name() {
        use crate::runtime::worktree::{BaseRef, Identity};

        let mut cfg = crate::config::Config::default();
        cfg.project.name = Some("myapp".into());
        let identity = Identity {
            slug: "feature-x".into(),
            base_ref: BaseRef::Branch("feature/x".into()),
            offset: 42,
        };
        let mut vars = BTreeMap::new();
        let mut keys = BTreeSet::new();
        vars.insert("COMPOSE_PROJECT_NAME".into(), "user-chose-this".into());
        inject_worktree_env(&mut vars, &mut keys, &cfg, &identity);
        assert_eq!(
            vars.get("COMPOSE_PROJECT_NAME").map(String::as_str),
            Some("user-chose-this")
        );
        // Still flagged project-declared even when value pre-existed —
        // keel owns the semantics of the name, so it must survive the
        // container-exec filter.
        assert!(keys.contains("COMPOSE_PROJECT_NAME"));
    }

    #[test]
    fn inject_worktree_env_skips_compose_when_unisolated() {
        use crate::runtime::worktree::Identity;

        let mut cfg = crate::config::Config::default();
        cfg.project.name = Some("myapp".into());
        let mut vars = BTreeMap::new();
        let mut keys = BTreeSet::new();
        inject_worktree_env(&mut vars, &mut keys, &cfg, &Identity::none());
        assert_eq!(vars.get("KEEL_WORKTREE_SLUG").map(String::as_str), Some(""));
        assert_eq!(
            vars.get("KEEL_WORKTREE_OFFSET").map(String::as_str),
            Some("0")
        );
        assert!(!vars.contains_key("COMPOSE_PROJECT_NAME"));
        assert!(!keys.contains("COMPOSE_PROJECT_NAME"));
    }

    #[test]
    fn inject_worktree_env_respects_isolate_compose_false() {
        use crate::runtime::worktree::{BaseRef, Identity};

        let mut cfg = crate::config::Config::default();
        cfg.project.name = Some("myapp".into());
        cfg.worktrees.isolate_compose = false;
        let identity = Identity {
            slug: "feature-x".into(),
            base_ref: BaseRef::Branch("feature/x".into()),
            offset: 1,
        };
        let mut vars = BTreeMap::new();
        let mut keys = BTreeSet::new();
        inject_worktree_env(&mut vars, &mut keys, &cfg, &identity);
        assert!(!vars.contains_key("COMPOSE_PROJECT_NAME"));
        // slug + offset still injected.
        assert_eq!(
            vars.get("KEEL_WORKTREE_SLUG").map(String::as_str),
            Some("feature-x")
        );
    }

    #[tokio::test]
    async fn base_offset_arithmetic_resolves() {
        use crate::runtime::worktree::{BaseRef, Identity};

        let cfg: crate::config::Config = crate::config::parse_str(
            r#"
            [env]
            APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
        "#,
        )
        .unwrap();
        let identity = Identity {
            slug: "pinned".into(),
            base_ref: BaseRef::Branch("pinned".into()),
            offset: 7,
        };
        let env =
            Env::resolve_with_identity(&cfg, std::env::current_dir().unwrap().as_path(), &identity)
                .await
                .unwrap();
        assert_eq!(env.get("APP_PORT"), Some("8087"));
    }

    #[tokio::test]
    async fn base_offset_falls_back_to_base_when_offset_var_missing() {
        let cfg: crate::config::Config = crate::config::parse_str(
            r#"
            [env]
            DB_PORT = { base = "5432", offset = "DOES_NOT_EXIST" }
        "#,
        )
        .unwrap();
        let env = Env::resolve_with_identity(
            &cfg,
            std::env::current_dir().unwrap().as_path(),
            &crate::runtime::worktree::Identity::none(),
        )
        .await
        .unwrap();
        assert_eq!(env.get("DB_PORT"), Some("5432"));
    }

    #[tokio::test]
    async fn base_offset_errors_on_non_integer_base() {
        let cfg: crate::config::Config = crate::config::parse_str(
            r#"
            [env]
            BAD = { base = "not-a-number" }
        "#,
        )
        .unwrap();
        let err = Env::resolve_with_identity(
            &cfg,
            std::env::current_dir().unwrap().as_path(),
            &crate::runtime::worktree::Identity::none(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RuntimeError::ArgvParse { .. }));
    }
}
