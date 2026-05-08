//! Loading configuration from disk.
//!
//! Encapsulates the TOML format choice. Callers (CLI, runtime, tui) work in
//! terms of [`Config`] and never speak TOML directly.

use crate::error::ConfigError;
use crate::model::Config;
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
}
