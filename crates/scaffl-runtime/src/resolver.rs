//! Command-name resolution.
//!
//! Given a name typed at the CLI, decide which dispatch path it follows.
//! Resolution is a pure function of the loaded [`Config`] plus a list of
//! known compose services and on-disk script names. Side-effect-free, so
//! it's the same in `scaffl which`, `scaffl --explain`, and the TUI palette.

use scaffl_config::Config;

/// The dispatch decision for a typed command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Built-in scaffl subcommand (handled by the CLI before reaching here).
    Builtin(&'static str),
    /// User-defined recipe in `scaffl.toml`.
    Recipe(&'a str),
    /// Script under `.scaffl/commands/<name>` (or `<name>.sh`).
    Script(&'a str),
    /// Pass remaining args through to `compose <name> ...`.
    ComposePassthrough(&'a str),
    /// Treat `<name>` as a service and exec the rest inside it.
    ServiceExec(&'a str),
    /// No match; carry an optional did-you-mean suggestion.
    Unknown { suggestion: Option<String> },
}

/// Inputs the resolver needs that are not in the config itself.
///
/// Scripts are resolved from `Config::scripts` (populated by
/// [`scaffl_config::load_project`]). Services and compose subcommands come
/// from outside the config because they aren't user-authored.
#[derive(Debug, Default, Clone)]
pub struct ResolverContext {
    /// Compose service names known from the project.
    pub services: Vec<String>,
    /// Compose subcommands eligible for passthrough (`ps`, `logs`, ...).
    pub compose_subcommands: Vec<&'static str>,
}

const BUILTINS: &[&str] = &[
    "init", "doctor", "ui", "hooks", "list", "ls", "which", "env", "help", "version",
];

const DEFAULT_COMPOSE_SUBCOMMANDS: &[&str] = &[
    "ps", "exec", "pull", "push", "run", "config", "events", "version", "top", "port", "pause",
    "unpause", "kill", "rm", "create", "start", "stop", "restart", "images", "cp", "logs", "build",
];

/// Pure resolver — depends only on its inputs.
pub struct Resolver<'a> {
    config: &'a Config,
    ctx: ResolverContext,
}

impl<'a> Resolver<'a> {
    pub fn new(config: &'a Config, mut ctx: ResolverContext) -> Self {
        if ctx.compose_subcommands.is_empty() {
            ctx.compose_subcommands = DEFAULT_COMPOSE_SUBCOMMANDS.to_vec();
        }
        Self { config, ctx }
    }

    /// Resolve a typed name to its dispatch path.
    pub fn resolve<'b>(&'b self, name: &'b str) -> Resolution<'b> {
        if let Some(b) = BUILTINS.iter().find(|b| **b == name) {
            return Resolution::Builtin(b);
        }
        if self.config.commands.contains_key(name) {
            return Resolution::Recipe(name);
        }
        if self.config.scripts.contains_key(name) {
            return Resolution::Script(name);
        }
        if self.config.runtime.compose_passthrough && self.ctx.compose_subcommands.contains(&name) {
            return Resolution::ComposePassthrough(name);
        }
        if self.config.runtime.service_passthrough && self.ctx.services.iter().any(|s| s == name) {
            return Resolution::ServiceExec(name);
        }
        Resolution::Unknown {
            suggestion: self.suggest(name),
        }
    }

    fn suggest(&self, typed: &str) -> Option<String> {
        let candidates = self.candidate_names();
        let typed_lower = typed.to_ascii_lowercase();
        candidates
            .into_iter()
            .min_by_key(|c| levenshtein(&typed_lower, &c.to_ascii_lowercase()))
            .filter(|c| levenshtein(&typed_lower, &c.to_ascii_lowercase()) <= 2)
    }

    fn candidate_names(&self) -> Vec<String> {
        let mut v = Vec::new();
        v.extend(BUILTINS.iter().map(|s| (*s).to_string()));
        v.extend(self.config.commands.keys().cloned());
        v.extend(self.config.scripts.keys().cloned());
        v.extend(self.ctx.services.iter().cloned());
        if self.config.runtime.compose_passthrough {
            v.extend(
                self.ctx
                    .compose_subcommands
                    .iter()
                    .map(|s| (*s).to_string()),
            );
        }
        v
    }
}

/// Iterative O(n*m) Levenshtein distance, allocating a single row buffer.
///
/// Inlined here rather than pulled from a crate because resolver suggestions
/// are the only consumer and the implementation is small.
fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use scaffl_config::Config;

    fn config_with(commands: &[&str]) -> Config {
        let mut src = String::new();
        for c in commands {
            src.push_str(&format!("[command.\"{c}\"]\nrun = \"echo {c}\"\n"));
        }
        scaffl_config::parse_str(&src).unwrap()
    }

    #[test]
    fn builtin_takes_priority() {
        let cfg = config_with(&["doctor"]); // even if user shadows with a recipe
        let r = Resolver::new(&cfg, ResolverContext::default());
        assert_eq!(r.resolve("doctor"), Resolution::Builtin("doctor"));
    }

    #[test]
    fn recipe_resolves() {
        let cfg = config_with(&["test"]);
        let r = Resolver::new(&cfg, ResolverContext::default());
        assert_eq!(r.resolve("test"), Resolution::Recipe("test"));
    }

    #[test]
    fn script_resolves_when_no_recipe() {
        use scaffl_config::ScriptCommand;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut cfg = config_with(&[]);
        cfg.scripts.insert(
            "seed".into(),
            ScriptCommand {
                name: "seed".into(),
                path: PathBuf::from("/dev/null"),
                desc: None,
                service: None,
                tty: false,
                env: BTreeMap::new(),
                needs: Vec::new(),
                forward_args: false,
            },
        );
        let r = Resolver::new(&cfg, ResolverContext::default());
        assert_eq!(r.resolve("seed"), Resolution::Script("seed"));
    }

    #[test]
    fn compose_passthrough_when_enabled() {
        let cfg = config_with(&[]);
        let r = Resolver::new(&cfg, ResolverContext::default());
        assert_eq!(r.resolve("ps"), Resolution::ComposePassthrough("ps"));
    }

    #[test]
    fn service_exec_when_enabled_and_known_service() {
        let cfg = config_with(&[]);
        let r = Resolver::new(
            &cfg,
            ResolverContext {
                services: vec!["app".into()],
                ..Default::default()
            },
        );
        assert_eq!(r.resolve("app"), Resolution::ServiceExec("app"));
    }

    #[test]
    fn recipe_outranks_compose_passthrough() {
        let cfg = config_with(&["ps"]);
        let r = Resolver::new(&cfg, ResolverContext::default());
        assert_eq!(r.resolve("ps"), Resolution::Recipe("ps"));
    }

    #[test]
    fn unknown_suggests_close_match() {
        let cfg = config_with(&["migrate", "test"]);
        let r = Resolver::new(&cfg, ResolverContext::default());
        let res = r.resolve("migate");
        match res {
            Resolution::Unknown {
                suggestion: Some(s),
            } => assert_eq!(s, "migrate"),
            _ => panic!("expected suggestion"),
        }
    }

    #[test]
    fn unknown_no_suggestion_when_far() {
        let cfg = config_with(&["migrate"]);
        let r = Resolver::new(&cfg, ResolverContext::default());
        let res = r.resolve("zzzzz");
        assert_eq!(res, Resolution::Unknown { suggestion: None });
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }
}
