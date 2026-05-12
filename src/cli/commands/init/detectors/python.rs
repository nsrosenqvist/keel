//! Python detector. Picks the package/run tool by lockfile and emits
//! test / install suggestions.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment};
use std::path::Path;

pub struct Python;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Uv,
    Poetry,
    Pdm,
    Pipenv,
    Pip,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Uv => "uv",
            Tool::Poetry => "poetry",
            Tool::Pdm => "pdm",
            Tool::Pipenv => "pipenv",
            Tool::Pip => "pip",
        }
    }
}

fn pick(root: &Path) -> Option<Tool> {
    let has = |p: &str| root.join(p).exists();
    if has("uv.lock") {
        return Some(Tool::Uv);
    }
    if has("poetry.lock") {
        return Some(Tool::Poetry);
    }
    if has("pdm.lock") {
        return Some(Tool::Pdm);
    }
    if has("Pipfile.lock") || has("Pipfile") {
        return Some(Tool::Pipenv);
    }
    if has("requirements.txt") {
        return Some(Tool::Pip);
    }
    // pyproject.toml alone → default to uv (modern Python default).
    if has("pyproject.toml") {
        return Some(Tool::Uv);
    }
    None
}

impl Detector for Python {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let tool = pick(root)?;
        let fragments = match tool {
            Tool::Uv => vec![
                Fragment::Command(CommandFragment::shell(
                    "install",
                    "Sync dependencies",
                    "uv sync",
                )),
                Fragment::Command(
                    CommandFragment::shell("test", "Run tests", "uv run pytest")
                        .with_forward_args(),
                ),
                Fragment::Command(CommandFragment::shell(
                    "lint",
                    "Run ruff",
                    "uv run ruff check",
                )),
            ],
            Tool::Poetry => vec![
                Fragment::Command(CommandFragment::shell(
                    "install",
                    "Install dependencies",
                    "poetry install",
                )),
                Fragment::Command(
                    CommandFragment::shell("test", "Run tests", "poetry run pytest")
                        .with_forward_args(),
                ),
            ],
            Tool::Pdm => vec![
                Fragment::Command(CommandFragment::shell(
                    "install",
                    "Install dependencies",
                    "pdm install",
                )),
                Fragment::Command(
                    CommandFragment::shell("test", "Run tests", "pdm run pytest")
                        .with_forward_args(),
                ),
            ],
            Tool::Pipenv => vec![
                Fragment::Command(CommandFragment::shell(
                    "install",
                    "Install dependencies",
                    "pipenv install --dev",
                )),
                Fragment::Command(
                    CommandFragment::shell("test", "Run tests", "pipenv run pytest")
                        .with_forward_args(),
                ),
            ],
            Tool::Pip => vec![
                Fragment::Command(CommandFragment::shell(
                    "install",
                    "Install dependencies",
                    "pip install -r requirements.txt",
                )),
                Fragment::Command(
                    CommandFragment::shell("test", "Run tests", "pytest").with_forward_args(),
                ),
            ],
        };
        Some(Finding {
            ecosystem: "python",
            tool: Some(tool.name().to_string()),
            fragments,
            notes: vec![format!("Detected Python project; using `{}`.", tool.name())],
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
    fn uv_lock_wins() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "pyproject.toml");
        touch(d.path(), "uv.lock");
        touch(d.path(), "poetry.lock");
        let f = Python.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("uv"));
    }

    #[test]
    fn poetry_over_pdm_over_pipenv_over_pip() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "poetry.lock");
        touch(d.path(), "pdm.lock");
        touch(d.path(), "Pipfile.lock");
        touch(d.path(), "requirements.txt");
        assert_eq!(
            Python.detect(d.path()).unwrap().tool.as_deref(),
            Some("poetry")
        );
    }

    #[test]
    fn bare_pyproject_defaults_to_uv() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "pyproject.toml");
        assert_eq!(Python.detect(d.path()).unwrap().tool.as_deref(), Some("uv"));
    }

    #[test]
    fn requirements_only_uses_pip() {
        let d = TempDir::new().unwrap();
        touch(d.path(), "requirements.txt");
        assert_eq!(
            Python.detect(d.path()).unwrap().tool.as_deref(),
            Some("pip")
        );
    }

    #[test]
    fn none_when_no_python_signals() {
        let d = TempDir::new().unwrap();
        assert!(Python.detect(d.path()).is_none());
    }
}
