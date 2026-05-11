//! Loading configuration from disk.
//!
//! Encapsulates the TOML format choice. Callers (CLI, runtime, tui) work in
//! terms of [`Config`] and never speak TOML directly.

use crate::error::ConfigError;
use crate::install::discover_install_steps;
use crate::model::Config;
use crate::scripts::ScriptCommand;
use std::collections::BTreeMap;
use std::path::Path;

/// Parse a [`Config`] from a TOML source string.
pub fn parse_str(source: &str) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(source).map_err(|source| ConfigError::Parse {
        path: Path::new("<inline>").to_path_buf(),
        source,
    })?;
    config.validate().map_err(ConfigError::Invalid)?;
    Ok(config)
}

/// Read a `keel.toml` (or other TOML file) from disk and parse it.
pub fn load_from_path(path: &Path) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let config: Config = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    config.validate().map_err(ConfigError::Invalid)?;
    Ok(config)
}

/// Load the full project configuration without applying a worktree
/// overlay. Equivalent to `load_project_with_slug(root, None)`.
pub fn load_project(project_root: &Path) -> Result<Config, ConfigError> {
    load_project_with_slug(project_root, None)
}

/// Load the full project configuration with an optional per-worktree
/// overlay.
///
/// Layered on top of each other (later wins):
///
/// 1. `keel.toml` at the project root.
/// 2. `.keel/local.toml` (per-developer overrides; gitignored).
/// 3. `.keel/worktrees/<slug>.toml` when `slug` is `Some` and the
///    file exists (per-worktree overrides; gitignored).
///
/// Missing `keel.toml` is fine — a default config is used as the
/// base. Plus: scripts under `.keel/commands/` are discovered after
/// merging.
///
/// Merging is done at the `toml::Value` level so any TOML structure
/// works: tables merge recursively, scalars and arrays replace.
pub fn load_project_with_slug(
    project_root: &Path,
    slug: Option<&str>,
) -> Result<Config, ConfigError> {
    let toml_path = project_root.join("keel.toml");
    let local_path = project_root.join(".keel").join("local.toml");

    let mut value = if toml_path.exists() {
        read_toml_value(&toml_path)?
    } else {
        toml::Value::Table(Default::default())
    };

    if local_path.exists() {
        let local = read_toml_value(&local_path)?;
        deep_merge(&mut value, local);
    }

    if let Some(slug) = slug.filter(|s| !s.is_empty()) {
        let overlay_path = project_root
            .join(".keel")
            .join("worktrees")
            .join(format!("{slug}.toml"));
        if overlay_path.exists() {
            let overlay = read_toml_value(&overlay_path)?;
            deep_merge(&mut value, overlay);
        }
    }

    let mut config: Config =
        value
            .try_into()
            .map_err(|source: toml::de::Error| ConfigError::Parse {
                path: toml_path.clone(),
                source,
            })?;

    let scripts_dir = project_root.join(".keel").join("commands");
    if scripts_dir.is_dir() {
        config.scripts = discover_scripts(&scripts_dir)?;
    }
    let install_dir = project_root.join(".keel").join("install");
    if install_dir.is_dir() {
        let (discovered, order) = discover_install_steps(&install_dir)?;
        config.install.discovered = discovered;
        config.install.discovered_order = order;
    }
    config.validate().map_err(ConfigError::Invalid)?;
    Ok(config)
}

fn read_toml_value(path: &Path) -> Result<toml::Value, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&raw).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Recursive merge: overlay tables onto base tables key-by-key; replace
/// for everything else (scalars, arrays). Public for tests; the loader
/// is the only intended caller.
pub(crate) fn deep_merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, overlay) => *slot = overlay,
    }
}

/// Scan a directory for script files and parse their frontmatter.
///
/// Hidden files (`.foo`), files starting with `_`, and entries that aren't
/// regular files are skipped. Anything else is parsed; a parse error
/// fails the whole scan rather than silently dropping the script.
pub fn discover_scripts(dir: &Path) -> Result<BTreeMap<String, ScriptCommand>, ConfigError> {
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| ConfigError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str.starts_with('_') {
            continue;
        }
        let path = entry.path();
        let cmd = ScriptCommand::from_path(&path)?;
        out.insert(cmd.name.clone(), cmd);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_str_roundtrips_minimal() {
        let cfg = parse_str(
            r#"
                [project]
                name = "x"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.project.name.as_deref(), Some("x"));
    }

    #[test]
    fn load_from_path_reads_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keel.toml");
        std::fs::write(
            &path,
            r#"
                [project]
                name = "from-disk"
            "#,
        )
        .unwrap();
        let cfg = load_from_path(&path).unwrap();
        assert_eq!(cfg.project.name.as_deref(), Some("from-disk"));
    }

    #[test]
    fn load_from_path_reports_path_in_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keel.toml");
        std::fs::write(&path, "[project\nname = bad").unwrap();
        let err = load_from_path(&path).unwrap_err();
        match err {
            ConfigError::Parse { path: p, .. } => assert_eq!(p, path),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn load_project_discovers_scripts() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "x"
            "#,
        )
        .unwrap();
        let cmds = root.path().join(".keel").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("seed"), "#!/bin/sh\n# @desc: Seed\necho hi\n").unwrap();
        std::fs::write(cmds.join("migrate.sh"), "#!/bin/sh\necho m\n").unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert!(cfg.scripts.contains_key("seed"));
        assert!(cfg.scripts.contains_key("migrate"));
        assert_eq!(cfg.scripts["seed"].desc.as_deref(), Some("Seed"));
    }

    #[test]
    fn load_project_works_without_toml() {
        let root = TempDir::new().unwrap();
        let cmds = root.path().join(".keel").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("seed"), "echo hi\n").unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert!(cfg.scripts.contains_key("seed"));
    }

    #[test]
    fn load_project_merges_local_overlay() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "base"

                [command.test]
                run = "composer test"

                [env]
                APP_PORT = { default = "80" }
            "#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".keel")).unwrap();
        std::fs::write(
            root.path().join(".keel").join("local.toml"),
            r#"
                [project]
                name = "overridden"

                [command.test]
                forward_args = true

                [command.local-only]
                run = "echo from-local"

                [env]
                APP_PORT = { default = "8080" }
                LOCAL_VAR = { value = "yes" }
            "#,
        )
        .unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert_eq!(cfg.project.name.as_deref(), Some("overridden"));
        // Existing recipe got an additional field; original `run` survives.
        let test_recipe = &cfg.commands["test"];
        assert!(test_recipe.forward_args);
        // New recipe added.
        assert!(cfg.commands.contains_key("local-only"));
        // Env: existing key updated, new key appended.
        assert_eq!(
            cfg.env.get("APP_PORT").unwrap().default.as_deref(),
            Some("8080")
        );
        assert_eq!(
            cfg.env.get("LOCAL_VAR").unwrap().value.as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn load_project_works_with_only_local() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join(".keel")).unwrap();
        std::fs::write(
            root.path().join(".keel").join("local.toml"),
            r#"
                [command.greet]
                run = "echo hi"
            "#,
        )
        .unwrap();
        let cfg = load_project(root.path()).unwrap();
        assert!(cfg.commands.contains_key("greet"));
    }

    #[test]
    fn load_project_with_slug_applies_overlay() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "base"

                [command.test]
                run = "composer test"
            "#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".keel").join("worktrees")).unwrap();
        std::fs::write(
            root.path()
                .join(".keel")
                .join("worktrees")
                .join("feature-x.toml"),
            r#"
                [command.test]
                forward_args = true

                [command.feature-only]
                run = "echo from-feature"
            "#,
        )
        .unwrap();

        let cfg = load_project_with_slug(root.path(), Some("feature-x")).unwrap();
        assert!(cfg.commands["test"].forward_args);
        assert!(cfg.commands.contains_key("feature-only"));

        // Without the slug, the overlay is invisible.
        let cfg_main = load_project_with_slug(root.path(), Some("main")).unwrap();
        assert!(!cfg_main.commands["test"].forward_args);
        assert!(!cfg_main.commands.contains_key("feature-only"));
    }

    #[test]
    fn load_project_with_slug_layers_after_local() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "base"
            "#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".keel").join("worktrees")).unwrap();
        std::fs::write(
            root.path().join(".keel").join("local.toml"),
            r#"
                [project]
                name = "from-local"
            "#,
        )
        .unwrap();
        std::fs::write(
            root.path().join(".keel").join("worktrees").join("x.toml"),
            r#"
                [project]
                name = "from-worktree"
            "#,
        )
        .unwrap();
        let cfg = load_project_with_slug(root.path(), Some("x")).unwrap();
        // Worktree overlay is applied last, so its name wins.
        assert_eq!(cfg.project.name.as_deref(), Some("from-worktree"));
    }

    #[test]
    fn load_project_with_slug_falls_back_when_overlay_absent() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "stable"
            "#,
        )
        .unwrap();
        // No overlay file present.
        let cfg = load_project_with_slug(root.path(), Some("missing-slug")).unwrap();
        assert_eq!(cfg.project.name.as_deref(), Some("stable"));
    }

    #[test]
    fn load_project_with_slug_none_matches_load_project() {
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("keel.toml"),
            r#"
                [project]
                name = "x"
            "#,
        )
        .unwrap();
        let a = load_project(root.path()).unwrap();
        let b = load_project_with_slug(root.path(), None).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn load_project_skips_hidden_and_underscored() {
        let root = TempDir::new().unwrap();
        let cmds = root.path().join(".keel").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join(".secret"), "echo nope\n").unwrap();
        std::fs::write(cmds.join("_helper.sh"), "echo nope\n").unwrap();
        std::fs::write(cmds.join("real"), "echo yes\n").unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert_eq!(cfg.scripts.len(), 1);
        assert!(cfg.scripts.contains_key("real"));
    }
}
