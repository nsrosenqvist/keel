//! Named `--template <stack>` scaffolds.
//!
//! These are hand-curated rich starter configs — independent of the
//! auto-detection registry. Each function returns a ready-to-edit
//! `keel.toml` body for a specific stack. New stacks are added by
//! extending [`Template`] and adding a matching function.

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Template {
    Laravel,
    Rails,
    Node,
    Rust,
}

pub fn render(project_name: &str, template: Template) -> String {
    match template {
        Template::Laravel => laravel_template(project_name),
        Template::Rails => rails_template(project_name),
        Template::Node => node_template(project_name),
        Template::Rust => rust_template(project_name),
    }
}

fn laravel_template(name: &str) -> String {
    format!(
        r#"# keel: Laravel + Docker Compose dev loop

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
        r#"# keel: Rails + Docker Compose dev loop

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
        r#"# keel: Node.js host-based dev loop

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
        r#"# keel: Rust workspace dev loop

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
