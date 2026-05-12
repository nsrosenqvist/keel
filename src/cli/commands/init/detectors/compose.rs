//! Docker Compose detector. Owns `[runtime] backend = "compose"`.

use crate::cli::commands::init::detector::{Detector, Finding, Fragment};
use std::path::Path;

pub struct Compose;

const FILES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

impl Detector for Compose {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let found = FILES.iter().find(|f| root.join(f).exists())?;
        Some(Finding {
            ecosystem: "compose",
            tool: Some((*found).to_string()),
            fragments: vec![Fragment::Runtime {
                backend: "compose",
                default_service: None,
                compose_passthrough: true,
                service_passthrough: true,
            }],
            notes: vec![format!("Detected {found}; backend = \"compose\".")],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_docker_compose_yml() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "").unwrap();
        let f = Compose.detect(d.path()).unwrap();
        assert_eq!(f.ecosystem, "compose");
        assert!(matches!(
            f.fragments[0],
            Fragment::Runtime {
                backend: "compose",
                ..
            }
        ));
    }

    #[test]
    fn detects_compose_yaml_variants() {
        for name in ["docker-compose.yaml", "compose.yml", "compose.yaml"] {
            let d = TempDir::new().unwrap();
            std::fs::write(d.path().join(name), "").unwrap();
            assert!(Compose.detect(d.path()).is_some(), "{name} should match");
        }
    }

    #[test]
    fn none_when_no_compose_file() {
        let d = TempDir::new().unwrap();
        assert!(Compose.detect(d.path()).is_none());
    }
}
