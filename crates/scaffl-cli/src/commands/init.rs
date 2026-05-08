//! `scaffl init` — scaffold a starter `scaffl.toml`.
//!
//! Non-interactive, idempotent (refuses to overwrite). Detects a few
//! signals from the project root and adjusts the template:
//!
//! - `docker-compose.{yml,yaml}` or `compose.{yml,yaml}` → `backend = "compose"`
//! - `.env` present → seed `[env_files]` with `[".env"]`
//! - `package.json` present → suggest an `npm` recipe in commented form
//! - `composer.json` present → suggest an `artisan` recipe in commented form

use anyhow::{Context, Result};
use std::path::Path;

pub fn run(project_root: &Path) -> Result<()> {
    let target = project_root.join("scaffl.toml");
    if target.exists() {
        anyhow::bail!("{} already exists; refusing to overwrite", target.display());
    }
    let signals = detect_signals(project_root);
    let body = render_template(project_root, &signals);
    std::fs::write(&target, body).with_context(|| format!("write {}", target.display()))?;
    println!("Wrote {}", target.display());
    if signals.compose {
        println!("Detected docker compose; backend = \"compose\".");
    }
    if signals.dotenv {
        println!("Detected .env; added to env_files.");
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
struct Signals {
    compose: bool,
    dotenv: bool,
    package_json: bool,
    composer_json: bool,
}

fn detect_signals(root: &Path) -> Signals {
    let exists = |name: &str| root.join(name).exists();
    Signals {
        compose: exists("docker-compose.yml")
            || exists("docker-compose.yaml")
            || exists("compose.yml")
            || exists("compose.yaml"),
        dotenv: exists(".env"),
        package_json: exists("package.json"),
        composer_json: exists("composer.json"),
    }
}

fn render_template(root: &Path, signals: &Signals) -> String {
    let project_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_string();

    let mut out = String::new();
    out.push_str("# scaffl configuration. See AGENTS.md / README.md for guidance.\n\n");

    out.push_str(&format!("[project]\nname = \"{project_name}\"\n\n"));

    out.push_str("[runtime]\n");
    if signals.compose {
        out.push_str("backend = \"compose\"\n");
        out.push_str("# default_service = \"app\"\n");
        out.push_str("compose_passthrough = true\n");
        out.push_str("service_passthrough = true\n");
    } else {
        out.push_str("backend = \"none\"\n");
    }
    out.push('\n');

    if signals.dotenv {
        out.push_str("[env_files]\nfiles = [\".env\"]\n\n");
    }

    out.push_str(
        "# Define commands. Run on the host by default; add `in = \"<service>\"`\n\
         # to exec inside a container service.\n\n",
    );

    if signals.compose {
        out.push_str(
            "# [command.up]\n\
             # desc = \"Start all services\"\n\
             # run  = \"docker compose up -d\"\n\n\
             # [command.shell]\n\
             # desc = \"Open a shell in the app container\"\n\
             # in   = \"app\"\n\
             # run  = \"/bin/sh\"\n\
             # tty  = true\n\n",
        );
    }

    if signals.composer_json {
        out.push_str(
            "# [command.test]\n\
             # desc         = \"Run test suite\"\n\
             # in           = \"app\"\n\
             # run          = \"composer test\"\n\
             # forward_args = true\n\n",
        );
    } else if signals.package_json {
        out.push_str(
            "# [command.test]\n\
             # desc         = \"Run test suite\"\n\
             # run          = \"npm test\"\n\
             # forward_args = true\n\n",
        );
    } else {
        out.push_str(
            "# [command.greet]\n\
             # desc = \"Replace me\"\n\
             # run  = \"echo hello\"\n\n",
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_template_when_absent() {
        let dir = TempDir::new().unwrap();
        run(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("[project]"));
        assert!(body.contains(&format!(
            "name = \"{}\"",
            dir.path().file_name().unwrap().to_string_lossy()
        )));
    }

    #[test]
    fn refuses_to_overwrite() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("scaffl.toml"), "existing").unwrap();
        let err = run(dir.path()).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap(),
            "existing"
        );
    }

    #[test]
    fn detects_compose_and_dotenv() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("docker-compose.yml"), "version: 3").unwrap();
        std::fs::write(dir.path().join(".env"), "FOO=bar").unwrap();
        run(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("backend = \"compose\""));
        assert!(body.contains("[env_files]"));
    }

    #[test]
    fn defaults_to_backend_none() {
        let dir = TempDir::new().unwrap();
        run(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("backend = \"none\""));
    }
}
