//! Node / Deno detector. Picks the package manager by lockfile and emits
//! dev / build / test / lint suggestions using its run-script syntax.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment};
use std::path::Path;

pub struct Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pm {
    Bun,
    Deno,
    Pnpm,
    Yarn,
    Npm,
}

impl Pm {
    fn name(self) -> &'static str {
        match self {
            Pm::Bun => "bun",
            Pm::Deno => "deno",
            Pm::Pnpm => "pnpm",
            Pm::Yarn => "yarn",
            Pm::Npm => "npm",
        }
    }

    /// How this tool invokes a named script from its manifest.
    /// (`<prefix> <script>`).
    fn run_prefix(self) -> &'static str {
        match self {
            Pm::Bun => "bun run",
            Pm::Deno => "deno task",
            Pm::Pnpm => "pnpm run",
            Pm::Yarn => "yarn",
            Pm::Npm => "npm run",
        }
    }
}

fn pick(root: &Path) -> Option<Pm> {
    let has = |p: &str| root.join(p).exists();
    if has("bun.lock") || has("bun.lockb") {
        return Some(Pm::Bun);
    }
    if has("deno.lock") || has("deno.json") || has("deno.jsonc") {
        return Some(Pm::Deno);
    }
    if !has("package.json") {
        return None;
    }
    if has("pnpm-lock.yaml") {
        return Some(Pm::Pnpm);
    }
    if has("yarn.lock") {
        return Some(Pm::Yarn);
    }
    Some(Pm::Npm)
}

impl Detector for Node {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let pm = pick(root)?;
        let prefix = pm.run_prefix();
        // `npm test` and `yarn test` are conventional shortcuts; the
        // others use the script-runner verb.
        let test_cmd = match pm {
            Pm::Npm => "npm test".to_string(),
            Pm::Yarn => "yarn test".to_string(),
            Pm::Bun => "bun test".to_string(),
            _ => format!("{prefix} test"),
        };
        let fragments = vec![
            Fragment::Command(CommandFragment::shell(
                "dev",
                "Start the dev server",
                format!("{prefix} dev"),
            )),
            Fragment::Command(CommandFragment::shell(
                "build",
                "Build for production",
                format!("{prefix} build"),
            )),
            Fragment::Command(
                CommandFragment::shell("test", "Run tests", test_cmd).with_forward_args(),
            ),
            Fragment::Command(CommandFragment::shell(
                "lint",
                "Run linters",
                format!("{prefix} lint"),
            )),
        ];
        Some(Finding {
            ecosystem: "node",
            tool: Some(pm.name().to_string()),
            fragments,
            notes: vec![format!(
                "Detected Node project; using `{}` for scripts.",
                pm.name()
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
    }

    #[test]
    fn bun_lock_wins_over_everything() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "package.json");
        touch(d.path(), "package-lock.json");
        touch(d.path(), "pnpm-lock.yaml");
        touch(d.path(), "yarn.lock");
        touch(d.path(), "bun.lock");
        let f = Node.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("bun"));
    }

    #[test]
    fn deno_detected_without_package_json() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "deno.json");
        let f = Node.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("deno"));
    }

    #[test]
    fn pnpm_over_yarn_over_npm() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "package.json");
        touch(d.path(), "pnpm-lock.yaml");
        touch(d.path(), "yarn.lock");
        let f = Node.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("pnpm"));
    }

    #[test]
    fn yarn_over_npm() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "package.json");
        touch(d.path(), "yarn.lock");
        let f = Node.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("yarn"));
    }

    #[test]
    fn falls_back_to_npm_with_just_package_json() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "package.json");
        let f = Node.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("npm"));
    }

    #[test]
    fn none_when_no_node_signals() {
        let d = TempDir::new().unwrap();
        assert!(Node.detect(d.path()).is_none());
    }

    #[test]
    fn deno_uses_task_verb() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "deno.json");
        let f = Node.detect(d.path()).unwrap();
        let dev = f
            .fragments
            .iter()
            .find_map(|fr| match fr {
                Fragment::Command(c) if c.name == "dev" => Some(c),
                _ => None,
            })
            .unwrap();
        match &dev.run {
            crate::cli::commands::init::detector::RunSpec::Single(s) => {
                assert!(s.starts_with("deno task"))
            }
            _ => panic!("expected single run"),
        }
    }
}
