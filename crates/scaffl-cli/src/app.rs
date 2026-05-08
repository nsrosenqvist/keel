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

    /// Activate a named profile for recipe execution. Profiles are
    /// declared as `[command.<name>.profile.<profile>]` in scaffl.toml.
    #[arg(long, global = true)]
    pub profile: Option<String>,

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
    Init {
        /// Use a specific stack template instead of auto-detection.
        #[arg(long)]
        template: Option<commands::init::Template>,
    },
    /// Open the TUI dashboard.
    Ui,
    /// Manage git hooks (install / run / uninstall).
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Emit a shell completion script (bash / zsh / fish / elvish / powershell).
    Completions { shell: clap_complete::Shell },
    /// Worktree identity, offset, and pinned-assignment management.
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    /// Re-run a recipe whenever watched files change.
    Watch {
        /// Recipe or script name.
        recipe: String,
        /// Args forwarded to the recipe.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Paths to watch (default: project root). Repeat the flag for
        /// multiple paths.
        #[arg(long)]
        path: Vec<PathBuf>,
        /// Debounce window in milliseconds (default: 300).
        #[arg(long)]
        debounce_ms: Option<u64>,
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

#[derive(Debug, Subcommand)]
pub enum WorktreeAction {
    /// Show the current worktree's identity, offset, and computed env.
    Status,
    /// List every git worktree with its computed offset.
    List,
    /// Pin a slug to a specific offset.
    Assign {
        /// Slug to pin (e.g. `main`, `feature/x`).
        name: String,
        /// Integer offset to assign.
        offset: u32,
        /// Write to .scaffl/local.toml (per-developer) instead of scaffl.toml.
        #[arg(long)]
        local: bool,
    },
}

pub async fn run(cli: Cli) -> Result<()> {
    init_tracing();

    // `init` and `completions` run before config load — `init` writes
    // the config that doesn't exist yet; `completions` should never fail
    // on a broken scaffl.toml because users may regenerate completions
    // *while fixing* such a file.
    if let Some(Command::Init { template }) = cli.command {
        let project_root = locate_project_root(cli.project.as_deref())?;
        return commands::init::run(&project_root, template);
    }
    if let Some(Command::Completions { shell }) = cli.command {
        return commands::completions::run(shell);
    }

    let project_root = locate_project_root(cli.project.as_deref())?;

    // Bootstrap pass: load scaffl.toml + local.toml so we can detect
    // the worktree identity using the user's `[worktrees]` settings
    // (modulus / seed / pinned assignments). Then load again with the
    // slug applied, so per-worktree overlays at
    // `.scaffl/worktrees/<slug>.toml` take effect.
    let bootstrap_cfg = scaffl_config::load_project_with_slug(&project_root, None)
        .with_context(|| format!("load project at {}", project_root.display()))?;
    let identity = scaffl_runtime::Identity::detect(&project_root, &bootstrap_cfg).await;
    let slug_for_overlay = if identity.is_isolated() {
        Some(identity.slug.as_str())
    } else {
        None
    };
    let config = scaffl_config::load_project_with_slug(&project_root, slug_for_overlay)
        .with_context(|| format!("load project at {}", project_root.display()))?;
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
            Command::Init { .. } => unreachable!("handled above"),
            Command::Completions { .. } => unreachable!("handled above"),
            Command::Ui => run_tui(Arc::clone(&cfg_arc), &project_root).await,
            Command::Watch {
                recipe,
                args,
                path,
                debounce_ms,
            } => {
                commands::watch::run(
                    Arc::clone(&cfg_arc),
                    &project_root,
                    recipe,
                    args,
                    path,
                    debounce_ms,
                )
                .await
            }
            Command::Worktree { action } => match action {
                WorktreeAction::Status => commands::worktree::status(&cfg_arc, &identity).await,
                WorktreeAction::List => commands::worktree::list(&cfg_arc, &project_root).await,
                WorktreeAction::Assign {
                    name,
                    offset,
                    local,
                } => commands::worktree::assign(&name, offset, local, &project_root),
            },
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
            let executor =
                build_executor(&cfg_arc, &project_root, &identity, cli.profile.as_deref()).await?;
            let owned = recipe_name.to_string();
            let code = executor.run_recipe(&owned, rest).await?;
            std::process::exit(code);
        }
        Resolution::Script(script_name) => {
            let executor =
                build_executor(&cfg_arc, &project_root, &identity, cli.profile.as_deref()).await?;
            let owned = script_name.to_string();
            let code = executor.run_script(&owned, rest).await?;
            std::process::exit(code);
        }
        Resolution::ComposePassthrough(sub) => {
            let executor =
                build_executor(&cfg_arc, &project_root, &identity, cli.profile.as_deref()).await?;
            let mut argv: Vec<&str> = vec![sub];
            argv.extend(rest.iter().map(String::as_str));
            let code = executor.passthrough(&argv).await?;
            std::process::exit(code);
        }
        Resolution::ServiceExec(service) => {
            let executor =
                build_executor(&cfg_arc, &project_root, &identity, cli.profile.as_deref()).await?;
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

/// Build a fully-configured Executor for CLI dispatch: detects the
/// backend, attaches the pre-resolved worktree identity, and applies an
/// optional profile.
async fn build_executor(
    config: &Arc<Config>,
    project_root: &Path,
    identity: &scaffl_runtime::Identity,
    profile: Option<&str>,
) -> Result<Executor> {
    let backend = build_backend(config).await?;
    let mut executor =
        Executor::new(backend, Arc::clone(config), project_root).with_identity(identity.clone());
    if let Some(p) = profile {
        executor = executor.with_profile(p);
    }
    Ok(executor)
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
    let executor =
        scaffl_runtime::Executor::new(Arc::clone(&backend), Arc::clone(&config), project_root);
    scaffl_tui::run(config, executor, backend, project_root)
        .await
        .context("run TUI")
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
