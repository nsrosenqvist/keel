//! Environment variable resolution for recipe execution.
//!
//! The merge order, from least to most specific (later wins):
//!
//! 1. Inherited process env.
//! 2. Project `[env]` section, with `default` / `value` / existing-env precedence.
//! 3. Recipe-level `env = {...}`.
//!
//! `from_command` and `.env` file loading land in a later phase; they slot
//! in at step 2 without changing the public shape of [`Env`].

use scaffl_config::{Config, EnvSpec};
use std::collections::BTreeMap;

/// Resolved environment ready to hand to a process.
#[derive(Debug, Default, Clone)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a base environment from the current process plus the project's
    /// `[env]` section. Each [`EnvSpec`] is resolved as:
    /// `value` → existing process env → `default`.
    pub fn from_config(config: &Config) -> Self {
        let mut vars: BTreeMap<String, String> = std::env::vars().collect();
        for (name, spec) in &config.env {
            if let Some(resolved) = resolve_spec(name, spec, &vars) {
                vars.insert(name.clone(), resolved);
            }
        }
        Self { vars }
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

    /// Iterate over the merged vars in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Consume into the underlying map.
    pub fn into_map(self) -> BTreeMap<String, String> {
        self.vars
    }
}

fn resolve_spec(name: &str, spec: &EnvSpec, existing: &BTreeMap<String, String>) -> Option<String> {
    if let Some(value) = &spec.value {
        return Some(value.clone());
    }
    if let Some(found) = existing.get(name) {
        return Some(found.clone());
    }
    spec.default.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use scaffl_config::{Config, EnvSpec};

    fn cfg_with_env(specs: &[(&str, EnvSpec)]) -> Config {
        let mut c = Config::default();
        for (k, v) in specs {
            c.env.insert((*k).to_string(), v.clone());
        }
        c
    }

    #[test]
    fn explicit_value_wins() {
        // SAFETY: tests run single-threaded by default for env mutation.
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
        let env = Env::from_config(&cfg);
        assert_eq!(env.get("SCAFFL_TEST_VAR"), Some("from-config"));
        unsafe {
            std::env::remove_var("SCAFFL_TEST_VAR");
        }
    }

    #[test]
    fn default_used_when_no_existing() {
        let cfg = cfg_with_env(&[(
            "SCAFFL_NO_VAR_PROBABLY",
            EnvSpec {
                value: None,
                default: Some("fallback".into()),
                from_command: None,
                required: false,
            },
        )]);
        // Make sure it's not set
        unsafe {
            std::env::remove_var("SCAFFL_NO_VAR_PROBABLY");
        }
        let env = Env::from_config(&cfg);
        assert_eq!(env.get("SCAFFL_NO_VAR_PROBABLY"), Some("fallback"));
    }

    #[test]
    fn recipe_overrides_win() {
        let cfg = cfg_with_env(&[(
            "APP_ENV",
            EnvSpec {
                value: Some("development".into()),
                default: None,
                from_command: None,
                required: false,
            },
        )]);
        let env = Env::from_config(&cfg).with_overrides([("APP_ENV", "testing")]);
        assert_eq!(env.get("APP_ENV"), Some("testing"));
    }
}
