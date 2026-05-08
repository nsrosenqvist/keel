//! CLI application wiring.

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "scaffl",
    version,
    about = "Dev-loop wrapper that adapts to your project"
)]
pub struct Cli {
    /// Path to the project root (default: search upward from cwd).
    #[arg(long, global = true)]
    pub project: Option<std::path::PathBuf>,

    /// Print the resolution path without executing.
    #[arg(long, global = true)]
    pub explain: bool,

    /// Subcommand or recipe name. Falls through to runtime dispatch.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn run(cli: Cli) -> Result<()> {
    init_tracing();

    if cli.args.is_empty() {
        // TODO: open the TUI dashboard once `scaffl-tui` ships its app.
        println!("scaffl: no command given (TUI launch pending implementation)");
        return Ok(());
    }

    let project_root = locate_project_root(cli.project.as_deref())?;
    let config_path = project_root.join("scaffl.toml");

    let config = if config_path.exists() {
        scaffl_config::load_from_path(&config_path)
            .with_context(|| format!("load {}", config_path.display()))?
    } else {
        scaffl_config::Config::default()
    };

    println!(
        "scaffl: project = {}, recipes = {}",
        project_root.display(),
        config.commands.len()
    );
    if cli.explain {
        println!("(--explain not yet implemented)");
    }
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .try_init();
}

fn locate_project_root(explicit: Option<&std::path::Path>) -> Result<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("scaffl.toml").exists() || cur.join(".scaffl").is_dir() {
            return Ok(cur);
        }
        if !cur.pop() {
            // No marker found — fall back to the original cwd.
            return Ok(std::env::current_dir()?);
        }
    }
}
