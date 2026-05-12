//! Rust / Cargo detector. Contributes build/test/fmt/clippy commands.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment, RunSpec};
use std::path::Path;

pub struct Rust;

impl Detector for Rust {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let cargo = root.join("Cargo.toml");
        if !cargo.exists() {
            return None;
        }
        let workspace = std::fs::read_to_string(&cargo)
            .ok()
            .is_some_and(|s| s.contains("[workspace]"));
        let scope = if workspace { " --workspace" } else { "" };

        let fragments = vec![
            Fragment::Command(
                CommandFragment::shell("build", "Build", format!("cargo build{scope}"))
                    .with_forward_args(),
            ),
            Fragment::Command(
                CommandFragment::shell("test", "Run tests", format!("cargo test{scope}"))
                    .with_forward_args(),
            ),
            Fragment::Command(CommandFragment::shell(
                "fmt",
                "Format code",
                "cargo fmt --all",
            )),
            Fragment::Command(CommandFragment {
                name: "check".into(),
                desc: "Format check + clippy + test".into(),
                run: RunSpec::Steps(vec![
                    "cargo fmt --all --check".into(),
                    format!("cargo clippy{scope} --all-targets -- -D warnings"),
                    "test".into(),
                ]),
                in_service: None,
                tty: None,
                forward_args: None,
                needs: Vec::new(),
            }),
        ];
        Some(Finding {
            ecosystem: "rust",
            tool: Some("cargo".into()),
            fragments,
            notes: vec![format!(
                "Detected Cargo.toml{}; suggested cargo commands.",
                if workspace { " (workspace)" } else { "" }
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_single_crate() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let f = Rust.detect(d.path()).unwrap();
        // 4 commands.
        assert_eq!(
            f.fragments
                .iter()
                .filter(|f| matches!(f, Fragment::Command(_)))
                .count(),
            4
        );
        // Non-workspace → no `--workspace` flag.
        let has_workspace_flag = f.fragments.iter().any(|frag| match frag {
            Fragment::Command(c) => match &c.run {
                RunSpec::Single(s) => s.contains("--workspace"),
                RunSpec::Steps(_) => false,
            },
            _ => false,
        });
        assert!(!has_workspace_flag);
    }

    #[test]
    fn detects_workspace() {
        let d = TempDir::new().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .unwrap();
        let f = Rust.detect(d.path()).unwrap();
        let has_workspace_flag = f.fragments.iter().any(|frag| match frag {
            Fragment::Command(c) => match &c.run {
                RunSpec::Single(s) => s.contains("--workspace"),
                _ => false,
            },
            _ => false,
        });
        assert!(has_workspace_flag);
    }

    #[test]
    fn none_when_absent() {
        let d = TempDir::new().unwrap();
        assert!(Rust.detect(d.path()).is_none());
    }
}
