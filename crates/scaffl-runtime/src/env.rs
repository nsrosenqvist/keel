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
//! [`Executor`](crate::Executor) caches the result in a [`tokio::sync::OnceCell`]
//! so multi-step or dependency-chained recipes pay the cost once per
//! invocation.

use crate::error::RuntimeError;
use scaffl_config::{Config, EnvSpec};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::process::Command;

/// Resolved environment ready to hand to a process.
#[derive(Debug, Default, Clone)]
pub struct Env {
    vars: BTreeMap<String, String>,
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
        }
    }

    /// Resolve the project base env: process + dotenv + `[env]` section.
    pub async fn resolve(config: &Config, project_root: &Path) -> Result<Self, RuntimeError> {
        let mut vars: BTreeMap<String, String> = std::env::vars().collect();

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
                vars.insert(k, v);
            }
        }

        for (name, spec) in &config.env {
            if let Some(value) = resolve_spec(name, spec, &vars).await? {
                vars.insert(name.clone(), value);
            }
        }

        Ok(Self { vars })
    }

    /// Apply per-recipe overrides on top of the base env.
    pub fn with_overrides<I, K, V>(mut self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in overrides {
            self.vars.insert(k.into(), v.into());
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

    /// Number of resolved variables.
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

async fn resolve_spec(
    name: &str,
    spec: &EnvSpec,
    existing: &BTreeMap<String, String>,
) -> Result<Option<String>, RuntimeError> {
    if let Some(value) = &spec.value {
        return Ok(Some(value.clone()));
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
            if next == '{' {
                if let Some(close) = bytes[i + 2..].iter().position(|b| *b == b'}') {
                    let name = &s[i + 2..i + 2 + close];
                    if let Some(v) = vars.get(name) {
                        out.push_str(v);
                    }
                    i += 2 + close + 1;
                    continue;
                }
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
    use pretty_assertions::assert_eq;
    use scaffl_config::{Config, EnvSpec};
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
            "SCAFFL_TEST_REQ_MISSING",
            EnvSpec {
                value: None,
                default: None,
                from_command: None,
                required: true,
            },
        )]);
        unsafe {
            std::env::remove_var("SCAFFL_TEST_REQ_MISSING");
        }
        let err = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap_err();
        match err {
            RuntimeError::RequiredEnvMissing(name) => {
                assert_eq!(name, "SCAFFL_TEST_REQ_MISSING")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn explicit_value_wins_over_existing() {
        unsafe {
            std::env::set_var("SCAFFL_TEST_VAR", "from-process");
        }
        let cfg = cfg_with_env(&[(
            "SCAFFL_TEST_VAR",
            EnvSpec {
                value: Some("from-config".into()),
                default: None,
                from_command: None,
                required: false,
            },
        )]);
        let env = Env::resolve(&cfg, std::env::current_dir().unwrap().as_path())
            .await
            .unwrap();
        assert_eq!(env.get("SCAFFL_TEST_VAR"), Some("from-config"));
        unsafe {
            std::env::remove_var("SCAFFL_TEST_VAR");
        }
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
}
