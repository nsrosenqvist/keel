//! CLI application wiring.

use crate::commands;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use scaffl_config::Config;
use scaffl_container::{Backend, compose::ComposeBackend};
use scaffl_runtime::{Executor, Resolution, Resolver, ResolverContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "scaffl",
    version,
    about = "Dev-loop wrapper that adapts to your project"
)]
pub struct Cli {
    /// Path to the project root (default: search upward from cwd).
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,

    /// Print the resolution path without executing.
    #[arg(long, global = true)]
    pub explain: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// Recipe name and args; used when no explicit subcommand is given.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List available recipes and scripts.
    #[command(alias = "ls")]
    List,
    /// Show how a name resolves (recipe, script, compose, service, none).
    Which { name: String },
    /// Print the resolved project environment (process + .env + [env]).
    Env,
    /// Validate the configuration and report on backend / deps / env files.
    Doctor,
    /// Scaffold a starter scaffl.toml in the project root.
    Init,
    /// Open the TUI dashboard.
    Ui,
    /// Manage git hooks (install / run / uninstall).
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Install scaffl-managed git hook shims.
    Install {
        /// Stages to install (default: pre-commit).
        #[arg(long, value_delimiter = ',')]
        stages: Vec<String>,
    },
    /// Remove scaffl-managed git hook shims.
    Uninstall {
        /// Stages to remove (default: all known stages).
        #[arg(long, value_delimiter = ',')]
        stages: Vec<String>,
    },
    /// Run hooks for a stage. Used by the installed shims; `git commit`
    /// invokes this via .git/hooks/pre-commit.
    Run { stage: String },
}

pub async fn run(cli: Cli) -> Result<()> {
    init_tracing();

    // `init` runs before config load — the whole point is to write the
    // config that doesn't exist yet.
    if matches!(cli.command, Some(Command::Init)) {
        let project_root = locate_project_root(cli.project.as_deref())?;
        return commands::init::run(&project_root);
    }

    let project_root = locate_project_root(cli.project.as_deref())?;
    let config = load_config(&project_root)?;
    let cfg_arc = Arc::new(config);

    if let Some(sub) = cli.command {
        return match sub {
            Command::List => cmd_list(&cfg_arc),
            Command::Which { name } => cmd_which(&cfg_arc, &name),
            Command::Env => commands::env::run(&cfg_arc, &project_root).await,
            Command::Doctor => {
                let code = commands::doctor::run(&cfg_arc, &project_root).await?;
                std::process::exit(code);
            }
            Command::Init => unreachable!("handled above"),
            Command::Ui => run_tui(Arc::clone(&cfg_arc), &project_root).await,
            Command::Hooks { action } => match action {
                HooksAction::Install { stages } => {
                    commands::hooks::install(&project_root, &stages).await
                }
                HooksAction::Uninstall { stages } => {
                    commands::hooks::uninstall(&project_root, &stages).await
                }
                HooksAction::Run { stage } => {
                    let code = commands::hooks::run(&cfg_arc, &project_root, &stage).await?;
                    std::process::exit(code);
                }
            },
        };
    }

    if cli.args.is_empty() {
        return run_tui(Arc::clone(&cfg_arc), &project_root).await;
    }

    let (name, rest) = split_args(&cli.args);

    let ctx = ResolverContext::default();
    let resolver = Resolver::new(&cfg_arc, ctx);
    let resolution = resolver.resolve(name);

    if cli.explain {
        return print_explain(name, &resolution);
    }

    match resolution {
        Resolution::Builtin(b) => {
            anyhow::bail!("built-in `{b}` not yet implemented");
        }
        Resolution::Recipe(recipe_name) => {
            let backend = build_backend(&cfg_arc).await?;
            let executor = Executor::new(backend, Arc::clone(&cfg_arc), &project_root);
            let owned = recipe_name.to_string();
            let code = executor.run_recipe(&owned, rest).await?;
            std::process::exit(code);
        }
        Resolution::Script(script_name) => {
            let backend = build_backend(&cfg_arc).await?;
            let executor = Executor::new(backend, Arc::clone(&cfg_arc), &project_root);
            let owned = script_name.to_string();
            let code = executor.run_script(&owned, rest).await?;
            std::process::exit(code);
        }
        Resolution::ComposePassthrough(sub) => {
            let backend = build_backend(&cfg_arc).await?;
            let executor = Executor::new(backend, Arc::clone(&cfg_arc), &project_root);
            let mut argv: Vec<&str> = vec![sub];
            argv.extend(rest.iter().map(String::as_str));
            let code = executor.passthrough(&argv).await?;
            std::process::exit(code);
        }
        Resolution::ServiceExec(service) => {
            let backend = build_backend(&cfg_arc).await?;
            let executor = Executor::new(backend, Arc::clone(&cfg_arc), &project_root);
            let argv: Vec<&str> = rest.iter().map(String::as_str).collect();
            let code = executor.service_exec(service, &argv, true).await?;
            std::process::exit(code);
        }
        Resolution::Unknown { suggestion } => {
            if let Some(s) = suggestion {
                anyhow::bail!("no such command `{name}` — did you mean `{s}`?");
            }
            anyhow::bail!("no such command `{name}`");
        }
    }
}

fn cmd_list(config: &Config) -> Result<()> {
    use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};

    if config.commands.is_empty() && config.scripts.is_empty() {
        println!("No recipes or scripts defined.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["name", "kind", "in", "description"]);
    for (name, recipe) in &config.commands {
        table.add_row(vec![
            name.clone(),
            "recipe".into(),
            recipe.service.clone().unwrap_or_else(|| "host".into()),
            recipe.desc.clone().unwrap_or_default(),
        ]);
    }
    for (name, script) in &config.scripts {
        table.add_row(vec![
            name.clone(),
            "script".into(),
            script.service.clone().unwrap_or_else(|| "host".into()),
            script.desc.clone().unwrap_or_default(),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn cmd_which(config: &Config, name: &str) -> Result<()> {
    let resolver = Resolver::new(config, ResolverContext::default());
    print_explain(name, &resolver.resolve(name))
}

fn print_explain(name: &str, resolution: &Resolution<'_>) -> Result<()> {
    match resolution {
        Resolution::Builtin(b) => println!("{name} → built-in `{b}`"),
        Resolution::Recipe(_) => println!("{name} → recipe in scaffl.toml"),
        Resolution::Script(_) => println!("{name} → script in .scaffl/commands/"),
        Resolution::ComposePassthrough(_) => println!("{name} → docker compose passthrough"),
        Resolution::ServiceExec(_) => println!("{name} → exec into compose service"),
        Resolution::Unknown {
            suggestion: Some(s),
        } => {
            println!("{name} → unknown (did you mean `{s}`?)")
        }
        Resolution::Unknown { suggestion: None } => println!("{name} → unknown"),
    }
    Ok(())
}

fn split_args(args: &[String]) -> (&str, &[String]) {
    let (head, tail) = args.split_first().expect("non-empty args");
    (head.as_str(), tail)
}

async fn build_backend(config: &Config) -> Result<Arc<dyn Backend>> {
    use scaffl_config::model::Backend as B;
    match config.runtime.backend {
        B::None => Ok(Arc::new(scaffl_container::null::NullBackend)),
        B::Compose => Ok(Arc::new(
            ComposeBackend::detect()
                .await
                .context("detect compose backend")?,
        )),
        B::Docker | B::Podman => anyhow::bail!(
            "backend `{:?}` is configured but not yet implemented; use `compose` or `none`",
            config.runtime.backend
        ),
    }
}

async fn run_tui(config: Arc<Config>, project_root: &Path) -> Result<()> {
    // Pick the configured backend, falling back to NullBackend on detection
    // failure so the TUI is still browseable on systems without compose.
    let backend: Arc<dyn Backend> = match build_backend(&config).await {
        Ok(b) => b,
        Err(_) => Arc::new(scaffl_container::null::NullBackend),
    };
    let executor = scaffl_runtime::Executor::new(backend, Arc::clone(&config), project_root);
    scaffl_tui::run(config, executor).await.context("run TUI")
}

fn load_config(project_root: &Path) -> Result<Config> {
    scaffl_config::load_project(project_root)
        .with_context(|| format!("load project at {}", project_root.display()))
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

fn locate_project_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("scaffl.toml").exists() || cur.join(".scaffl").is_dir() {
            return Ok(cur);
        }
        if !cur.pop() {
            return Ok(std::env::current_dir()?);
        }
    }
}
