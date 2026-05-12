//! Devcontainer detector. Owns `[devcontainer]`.

use crate::cli::commands::init::detector::{Detector, Finding, Fragment};
use std::path::{Path, PathBuf};

pub struct Devcontainer;

const PATHS: &[&str] = &[".devcontainer/devcontainer.json", ".devcontainer.json"];

impl Detector for Devcontainer {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let found = PATHS.iter().find(|p| root.join(p).exists())?;
        Some(Finding {
            ecosystem: "devcontainer",
            tool: None,
            fragments: vec![Fragment::Devcontainer {
                path: PathBuf::from(*found),
            }],
            notes: vec![format!(
                "Detected {found}; enabled the [devcontainer] integration."
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_devcontainer_json_under_dir() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join(".devcontainer")).unwrap();
        std::fs::write(d.path().join(".devcontainer/devcontainer.json"), "{}").unwrap();
        let f = Devcontainer.detect(d.path()).unwrap();
        assert_eq!(f.ecosystem, "devcontainer");
        assert!(matches!(f.fragments[0], Fragment::Devcontainer { .. }));
    }

    #[test]
    fn detects_root_level_devcontainer_json() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join(".devcontainer.json"), "{}").unwrap();
        assert!(Devcontainer.detect(d.path()).is_some());
    }

    #[test]
    fn none_when_absent() {
        let d = TempDir::new().unwrap();
        assert!(Devcontainer.detect(d.path()).is_none());
    }
}
