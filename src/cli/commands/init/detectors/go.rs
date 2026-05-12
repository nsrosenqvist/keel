//! Go modules detector. Contributes build/test/run commands.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment};
use std::path::Path;

pub struct Go;

impl Detector for Go {
    fn detect(&self, root: &Path) -> Option<Finding> {
        if !root.join("go.mod").exists() {
            return None;
        }
        let fragments = vec![
            Fragment::Command(
                CommandFragment::shell("build", "Build", "go build ./...").with_forward_args(),
            ),
            Fragment::Command(
                CommandFragment::shell("test", "Run tests", "go test ./...").with_forward_args(),
            ),
            Fragment::Command(
                CommandFragment::shell("run", "Run the main package", "go run .")
                    .with_forward_args(),
            ),
            Fragment::Command(CommandFragment::shell("vet", "Run go vet", "go vet ./...")),
        ];
        Some(Finding {
            ecosystem: "go",
            tool: Some("go".into()),
            fragments,
            notes: vec!["Detected go.mod; suggested go commands.".into()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_go_mod() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("go.mod"), "module x\n").unwrap();
        let f = Go.detect(d.path()).unwrap();
        assert_eq!(f.ecosystem, "go");
        assert!(
            f.fragments
                .iter()
                .any(|f| matches!(f, Fragment::Command(c) if c.name == "test"))
        );
    }

    #[test]
    fn none_when_absent() {
        let d = TempDir::new().unwrap();
        assert!(Go.detect(d.path()).is_none());
    }
}
