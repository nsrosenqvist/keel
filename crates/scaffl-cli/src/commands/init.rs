//! `scaffl init` — scaffold a starter `scaffl.toml`.
//!
//! Non-interactive, idempotent (refuses to overwrite).
//!
//! Two modes:
//!
//! - **Auto** (default): detects signals from the project root —
//!   `docker-compose.{yml,yaml}` or `compose.{yml,yaml}` →
//!   `backend = "compose"`; `.env` → `[env_files]`; `package.json` /
//!   `composer.json` → suggested commands.
//! - **Templated** (`--template <stack>`): writes a richer ready-to-edit
//!   config tailored to a specific stack. Available templates: laravel,
//!   rails, node, rust.

use anyhow::{Context, Result};
use clap::ValueEnum;
use std::path::Path;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Template {
    Laravel,
    Rails,
    Node,
    Rust,
}

pub fn run(project_root: &Path, template: Option<Template>) -> Result<()> {
    let target = project_root.join("scaffl.toml");
    if target.exists() {
        anyhow::bail!("{} already exists; refusing to overwrite", target.display());
    }
    let body = match template {
        Some(t) => render_named_template(project_root, t),
        None => {
            let signals = detect_signals(project_root);
            let body = render_template(project_root, &signals);
            if signals.compose {
                println!("Detected docker compose; backend = \"compose\".");
            }
            if signals.dotenv {
                println!("Detected .env; added to env_files.");
            }
            body
        }
    };
    std::fs::write(&target, body).with_context(|| format!("write {}", target.display()))?;
    println!("Wrote {}", target.display());
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

fn render_named_template(root: &Path, template: Template) -> String {
    let project_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    match template {
        Template::Laravel => laravel_template(project_name),
        Template::Rails => rails_template(project_name),
        Template::Node => node_template(project_name),
        Template::Rust => rust_template(project_name),
    }
}

fn laravel_template(name: &str) -> String {
    format!(
        r#"# scaffl: Laravel + Docker Compose dev loop

[project]
name = "{name}"

[runtime]
backend             = "compose"
default_service     = "app"
compose_passthrough = true
service_passthrough = true

[env_files]
files = [".env", ".env.${{APP_ENV}}"]

[env]
APP_PORT      = {{ default = "80" }}
APP_SERVICE   = {{ default = "app" }}
WWWUSER       = {{ from_command = "id -u" }}
WWWGROUP      = {{ from_command = "id -g" }}

[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.down]
desc = "Stop all services"
run  = "docker compose down"

[command.shell]
desc = "Open a shell in the app container"
in   = "app"
run  = "/bin/sh"
tty  = true

[command.artisan]
desc         = "Run an artisan command"
in           = "app"
run          = "php artisan"
tty          = true
forward_args = true

[command.composer]
desc         = "Run a composer command"
in           = "app"
run          = "composer"
tty          = true
forward_args = true

[command.migrate]
desc         = "Run database migrations"
needs        = ["up"]
in           = "app"
run          = "php artisan migrate"
forward_args = true

[command.fresh]
desc  = "Fresh DB with seeds"
needs = ["up"]
in    = "app"
run   = "php artisan migrate:fresh --seed"

[command.test]
desc         = "Run the test suite"
needs        = ["up"]
in           = "app"
run          = "composer test"
forward_args = true

[command.test.profile.ci]
tty = false
env = {{ XDEBUG_MODE = "off", APP_ENV = "testing" }}

[hooks]
pre-commit = ["test"]
"#
    )
}

fn rails_template(name: &str) -> String {
    format!(
        r#"# scaffl: Rails + Docker Compose dev loop

[project]
name = "{name}"

[runtime]
backend             = "compose"
default_service     = "web"
compose_passthrough = true
service_passthrough = true

[env_files]
files = [".env"]

[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.down]
desc = "Stop all services"
run  = "docker compose down"

[command.shell]
desc = "Open a shell in the web container"
in   = "web"
run  = "/bin/bash"
tty  = true

[command.console]
desc = "Rails console"
in   = "web"
run  = "bin/rails console"
tty  = true

[command.routes]
desc         = "Print routes"
in           = "web"
run          = "bin/rails routes"
forward_args = true

[command.migrate]
desc         = "Run database migrations"
needs        = ["up"]
in           = "web"
run          = "bin/rails db:migrate"
forward_args = true

[command.test]
desc         = "Run RSpec / minitest"
needs        = ["up"]
in           = "web"
run          = "bin/rails test"
forward_args = true
"#
    )
}

fn node_template(name: &str) -> String {
    format!(
        r#"# scaffl: Node.js host-based dev loop

[project]
name = "{name}"

[runtime]
backend = "none"

[env_files]
files = [".env", ".env.local"]

[command.dev]
desc = "Start the dev server"
run  = "npm run dev"

[command.build]
desc = "Build for production"
run  = "npm run build"

[command.test]
desc         = "Run tests"
run          = "npm test"
forward_args = true

[command.lint]
desc = "Run linters"
run  = ["npm run lint", "npm run typecheck"]

[command.check]
desc = "Lint + test"
run  = ["lint", "test"]

[hooks]
pre-commit = ["lint"]
"#
    )
}

fn rust_template(name: &str) -> String {
    format!(
        r#"# scaffl: Rust workspace dev loop

[project]
name = "{name}"

[runtime]
backend = "none"

[command.build]
desc         = "Build all crates"
run          = "cargo build --workspace"
forward_args = true

[command.test]
desc         = "Run tests"
run          = "cargo test --workspace"
forward_args = true

[command.fmt]
desc = "Format code"
run  = "cargo fmt --all"

[command.clippy]
desc = "Run clippy with -D warnings"
run  = "cargo clippy --workspace --all-targets -- -D warnings"

[command.check]
desc = "Format check + clippy + test"
run  = ["cargo fmt --all --check", "clippy", "test"]

[command.test.profile.ci]
env = {{ RUST_BACKTRACE = "1", CI = "true" }}

[hooks]
pre-commit = ["check"]
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_template_when_absent() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), None).unwrap();
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
        let err = run(dir.path(), None).unwrap_err();
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
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("backend = \"compose\""));
        assert!(body.contains("[env_files]"));
    }

    #[test]
    fn defaults_to_backend_none() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), None).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("backend = \"none\""));
    }

    #[test]
    fn template_laravel_writes_richer_config() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Laravel)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("[command.artisan]"));
        assert!(body.contains("[command.test.profile.ci]"));
        scaffl_config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_node_uses_backend_none() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Node)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(body.contains("backend = \"none\""));
        scaffl_config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_rust_parses_back() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Rust)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        scaffl_config::parse_str(&body).unwrap();
    }

    #[test]
    fn template_rails_parses_back() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some(Template::Rails)).unwrap();
        let body = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        scaffl_config::parse_str(&body).unwrap();
    }
}
