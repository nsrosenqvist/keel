//! Script-based commands.
//!
//! Files under `.scaffl/commands/` become subcommands. Each one carries
//! optional metadata in a "frontmatter" block of `# @key: value` lines that
//! appear at the top of the file (after an optional shebang).
//!
//! Frontmatter keys mirror the [`Recipe`] fields that make sense for
//! scripts:
//!
//! - `desc` / `description` — help text
//! - `in` / `service` — service to exec inside (host if absent)
//! - `tty` — boolean, allocate a TTY when execing in a container
//! - `needs` — comma-separated list of recipes/scripts that must run first
//! - `env` — `KEY=VALUE`, may be repeated across multiple frontmatter lines
//! - `forward-args` / `forward_args` — boolean, append CLI args to the script
//!
//! Anything beyond the frontmatter block is the script body and is not
//! parsed by scaffl. The file is executed verbatim — by the host shell
//! when no `in =` is set, or piped into `<interpreter> -s` inside the
//! configured service when `in = "<service>"` is set. The interpreter
//! is determined from the script's shebang (`bash` / `zsh` / `sh`),
//! falling back to `sh`. `tty = true` is ignored for in-container
//! scripts because compose's `exec` rejects the combination of stdin
//! piping and TTY allocation.
//!
//! [`Recipe`]: crate::Recipe

use crate::error::ConfigError;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A single script-defined command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCommand {
    pub name: String,
    pub path: PathBuf,
    pub desc: Option<String>,
    pub service: Option<String>,
    pub tty: bool,
    pub env: BTreeMap<String, String>,
    pub needs: Vec<String>,
    pub forward_args: bool,
}

impl ScriptCommand {
    /// Build a [`ScriptCommand`] by reading and parsing frontmatter from
    /// the script at `path`. The command name is derived from the file
    /// stem.
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "script path {} has no usable file stem",
                    path.display()
                ))
            })?
            .to_string();
        let content = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut cmd = Self {
            name,
            path: path.to_path_buf(),
            desc: None,
            service: None,
            tty: false,
            env: BTreeMap::new(),
            needs: Vec::new(),
            forward_args: false,
        };
        parse_frontmatter(&content, &mut cmd, path)?;
        Ok(cmd)
    }
}

fn parse_frontmatter(
    content: &str,
    cmd: &mut ScriptCommand,
    path: &Path,
) -> Result<(), ConfigError> {
    let mut lines = content.lines();
    if let Some(first) = lines.clone().next()
        && first.starts_with("#!")
    {
        lines.next();
    }
    for line in lines {
        let trimmed = line.trim_end();
        let Some(rest) = trimmed.strip_prefix("# @") else {
            // Frontmatter ends at the first line that isn't `# @key: value`.
            break;
        };
        let Some((raw_key, raw_value)) = rest.split_once(':') else {
            break;
        };
        let key = raw_key.trim().to_ascii_lowercase().replace('-', "_");
        let value = raw_value.trim();
        apply_frontmatter_kv(&key, value, cmd, path)?;
    }
    Ok(())
}

fn apply_frontmatter_kv(
    key: &str,
    value: &str,
    cmd: &mut ScriptCommand,
    path: &Path,
) -> Result<(), ConfigError> {
    match key {
        "desc" | "description" => cmd.desc = Some(value.to_string()),
        "in" | "service" => cmd.service = Some(value.to_string()),
        "tty" => cmd.tty = parse_bool(value, key, path)?,
        "forward_args" => cmd.forward_args = parse_bool(value, key, path)?,
        "needs" => {
            cmd.needs.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        }
        "env" => {
            let (k, v) = value.split_once('=').ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "{} frontmatter `env` line must be KEY=VALUE, got `{}`",
                    path.display(),
                    value
                ))
            })?;
            cmd.env.insert(k.trim().into(), v.trim().into());
        }
        other => {
            return Err(ConfigError::Invalid(format!(
                "{} unknown frontmatter key `@{}`",
                path.display(),
                other
            )));
        }
    }
    Ok(())
}

fn parse_bool(value: &str, key: &str, path: &Path) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        other => Err(ConfigError::Invalid(format!(
            "{} frontmatter `{}` must be a boolean, got `{}`",
            path.display(),
            key,
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn write_script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn parses_full_frontmatter() {
        let dir = TempDir::new().unwrap();
        let p = write_script(
            &dir,
            "migrate.sh",
            "#!/usr/bin/env bash\n\
             # @desc: Run database migrations\n\
             # @in: app\n\
             # @needs: up, build\n\
             # @env: APP_ENV=production\n\
             # @env: KEY=val\n\
             # @forward-args: true\n\
             # @tty: yes\n\
             php artisan migrate \"$@\"\n",
        );
        let cmd = ScriptCommand::from_path(&p).unwrap();
        assert_eq!(cmd.name, "migrate");
        assert_eq!(cmd.desc.as_deref(), Some("Run database migrations"));
        assert_eq!(cmd.service.as_deref(), Some("app"));
        assert_eq!(cmd.needs, vec!["up", "build"]);
        assert_eq!(
            cmd.env.get("APP_ENV").map(String::as_str),
            Some("production")
        );
        assert_eq!(cmd.env.get("KEY").map(String::as_str), Some("val"));
        assert!(cmd.forward_args);
        assert!(cmd.tty);
    }

    #[test]
    fn no_extension_works() {
        let dir = TempDir::new().unwrap();
        let p = write_script(&dir, "seed", "#!/bin/sh\necho hi\n");
        let cmd = ScriptCommand::from_path(&p).unwrap();
        assert_eq!(cmd.name, "seed");
        assert!(cmd.desc.is_none());
    }

    #[test]
    fn frontmatter_stops_at_first_non_directive() {
        let dir = TempDir::new().unwrap();
        let p = write_script(
            &dir,
            "x.sh",
            "#!/bin/bash\n# @desc: A\nset -e\n# @desc: Should be ignored\n",
        );
        let cmd = ScriptCommand::from_path(&p).unwrap();
        assert_eq!(cmd.desc.as_deref(), Some("A"));
    }

    #[test]
    fn unknown_key_is_an_error() {
        let dir = TempDir::new().unwrap();
        let p = write_script(&dir, "x.sh", "# @foo: bar\n");
        let err = ScriptCommand::from_path(&p).unwrap_err();
        match err {
            ConfigError::Invalid(msg) => assert!(msg.contains("foo")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn malformed_env_is_an_error() {
        let dir = TempDir::new().unwrap();
        let p = write_script(&dir, "x.sh", "# @env: nope\n");
        let err = ScriptCommand::from_path(&p).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn invalid_bool_is_an_error() {
        let dir = TempDir::new().unwrap();
        let p = write_script(&dir, "x.sh", "# @tty: maybe\n");
        let err = ScriptCommand::from_path(&p).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn no_shebang_is_fine() {
        let dir = TempDir::new().unwrap();
        let p = write_script(&dir, "raw", "# @desc: noshebang\necho hi\n");
        let cmd = ScriptCommand::from_path(&p).unwrap();
        assert_eq!(cmd.desc.as_deref(), Some("noshebang"));
    }
}
