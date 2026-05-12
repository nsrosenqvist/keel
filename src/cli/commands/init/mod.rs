//! `ampelos init` — scaffold a starter `ampelos.toml`.
//!
//! Non-interactive, idempotent (refuses to overwrite).
//!
//! Two modes:
//!
//! - **Auto** (default): walks the `detectors` registry. Each detector
//!   inspects the project root and contributes typed fragments (runtime,
//!   env files, suggested commands); `render` weaves them into a single
//!   TOML body. Auto-detected commands are emitted commented — the user
//!   uncomments what they want.
//! - **Templated** (`--template <stack>`): writes a richer hand-curated
//!   config for a specific stack (laravel, rails, node, rust). Bypasses
//!   the detector registry entirely. See the `template` submodule.

mod detector;
mod detectors;
mod render;
mod template;

pub use template::Template;

use anyhow::{Context, Result};
use std::path::Path;

pub fn run(project_root: &Path, template: Option<Template>) -> Result<()> {
    let target = project_root.join("ampelos.toml");
    if target.exists() {
        anyhow::bail!("{} already exists; refusing to overwrite", target.display());
    }
    let project_name = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");

    let body = match template {
        Some(t) => template::render(project_name, t),
        None => render_auto(project_root, project_name),
    };

    std::fs::write(&target, body).with_context(|| format!("write {}", target.display()))?;
    println!("Wrote {}", target.display());
    Ok(())
}

fn render_auto(project_root: &Path, project_name: &str) -> String {
    let findings: Vec<detector::Finding> = detectors::registry()
        .iter()
        .filter_map(|d| d.detect(project_root))
        .collect();
    for finding in &findings {
        for note in &finding.notes {
            println!("{note}");
        }
    }
    render::render(project_name, &findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_template_when_absent() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("[project]"));
        assert!(body.contains(&format!(
            "name = \"{}\"",
            dir.path().file_name().unwrap().to_string_lossy()
        )));
    }

    #[test]
    fn refuses_to_overwrite() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("ampelos.toml"), "existing").unwrap();
        let err = run(dir.path(), None).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap(),
            "existing"
        );
    }

    #[test]
    fn detects_compose_and_dotenv() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("docker-compose.yml"), "version: 3").unwrap();
        std::fs::write(dir.path().join(".env"), "FOO=bar").unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("backend = \"compose\""));
        assert!(body.contains("[env_files]"));
    }

    #[test]
    fn defaults_to_backend_none() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("backend = \"none\""));
    }

    #[test]
    fn mixed_repo_compose_owns_runtime_languages_only_emit_commands() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("docker-compose.yml"), "version: 3").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(dir.path().join(".env"), "").unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("backend = \"compose\""));
        assert!(body.contains("[env_files]"));
        assert!(body.contains("# [command.build]"));
        // Detected commands are commented.
        assert!(
            !body
                .lines()
                .any(|l| l.trim_start().starts_with("[command."))
        );
        crate::config::parse_str(&body).unwrap();
    }

    #[test]
    fn duplicate_command_name_across_ecosystems_emits_both_with_header() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("composer.json"), "{}").unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("Multiple ecosystems suggest `test`"));
        assert_eq!(body.matches("# [command.test]").count(), 2);
        crate::config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_laravel_writes_richer_config() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Laravel)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("[command.artisan]"));
        assert!(body.contains("[command.test.profile.ci]"));
        crate::config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_node_uses_backend_none() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Node)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        assert!(body.contains("backend = \"none\""));
        crate::config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_rust_parses_back() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Rust)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        crate::config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_rails_parses_back() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Rails)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("ampelos.toml")).unwrap();
        crate::config::parse_str(&body).unwrap();
    }
}
