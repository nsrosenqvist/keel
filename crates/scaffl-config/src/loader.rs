//! Loading configuration from disk.
//!
//! Encapsulates the TOML format choice. Callers (CLI, runtime, tui) work in
//! terms of [`Config`] and never speak TOML directly.

use crate::error::ConfigError;
use crate::model::Config;
use crate::scripts::ScriptCommand;
use std::collections::BTreeMap;
use std::path::Path;

/// Parse a [`Config`] from a TOML source string.
pub fn parse_str(source: &str) -> Result<Config, ConfigError> {
    toml::from_str(source).map_err(|source| ConfigError::Parse {
        path: Path::new("<inline>").to_path_buf(),
        source,
    })
}

/// Read a `scaffl.toml` (or other TOML file) from disk and parse it.
pub fn load_from_path(path: &Path) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&raw).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Load the full project configuration: `scaffl.toml` (if present) plus any
/// scripts discovered under `.scaffl/commands/`.
///
/// Missing `scaffl.toml` is fine — a default Config is returned and the
/// commands directory still scanned. This is the loader the CLI uses.
pub fn load_project(project_root: &Path) -> Result<Config, ConfigError> {
    let toml_path = project_root.join("scaffl.toml");
    let mut config = if toml_path.exists() {
        load_from_path(&toml_path)?
    } else {
        Config::default()
    };
    let scripts_dir = project_root.join(".scaffl").join("commands");
    if scripts_dir.is_dir() {
        config.scripts = discover_scripts(&scripts_dir)?;
    }
    Ok(config)
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
        let path = dir.path().join("scaffl.toml");
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
        let path = dir.path().join("scaffl.toml");
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
            root.path().join("scaffl.toml"),
            r#"
                [project]
                name = "x"
            "#,
        )
        .unwrap();
        let cmds = root.path().join(".scaffl").join("commands");
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
        let cmds = root.path().join(".scaffl").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("seed"), "echo hi\n").unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert!(cfg.scripts.contains_key("seed"));
    }

    #[test]
    fn load_project_skips_hidden_and_underscored() {
        let root = TempDir::new().unwrap();
        let cmds = root.path().join(".scaffl").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join(".secret"), "echo nope\n").unwrap();
        std::fs::write(cmds.join("_helper.sh"), "echo nope\n").unwrap();
        std::fs::write(cmds.join("real"), "echo yes\n").unwrap();

        let cfg = load_project(root.path()).unwrap();
        assert_eq!(cfg.scripts.len(), 1);
        assert!(cfg.scripts.contains_key("real"));
    }
}
