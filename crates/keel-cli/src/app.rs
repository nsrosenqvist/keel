//! CLI application wiring.

use crate::commands;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use keel_config::Config;
use keel_container::devcontainer::{DevcontainerBackend, DevcontainerIdentity, DevcontainerSpec};
use keel_container::{Backend, compose::ComposeBackend};
use keel_runtime::{Executor, Resolution, Resolver, ResolverContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "keel",
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
    /// declared as `[command.<name>.profile.<profile>]` in keel.toml.
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
    Env {
        /// Write the result to a dotenv file using a keel-managed
        /// block, instead of printing to stdout. Existing user content
        /// outside the block is preserved. Hook this up from
        /// post-checkout / post-merge so worktree-derived values land
        /// in `.env` and any tool that reads dotenv (compose, IDEs,
        /// rails, npm scripts, …) sees them.
        #[arg(long, value_name = "PATH")]
        write: Option<PathBuf>,
    },
    /// Validate the configuration and report on backend / deps / env files.
    Doctor,
    /// Scaffold a starter keel.toml in the project root.
    Init {
        /// Use a specific stack template instead of auto-detection.
        #[arg(long)]
        template: Option<commands::init::Template>,
    },
    /// Run the project's install steps (first-time setup, idempotent on re-run).
    Install {
        /// Optional step name — run only that step in isolation. Useful
        /// when a maintainer adds one new step that every teammate
        /// needs to apply (migration-style).
        step: Option<String>,
        /// Non-interactive resume from the first unresolved step.
        #[arg(long, conflicts_with_all = ["restart", "step"])]
        resume: bool,
        /// Wipe install state and run every step from scratch.
        #[arg(long, conflicts_with_all = ["resume", "step"])]
        restart: bool,
        /// Print the resolved step plan without executing.
        #[arg(long)]
        dry_run: bool,
        /// Print the plan plus the last-known status per step.
        #[arg(long)]
        list: bool,
        /// Force-refresh external hook repo caches.
        #[arg(long)]
        update_hooks: bool,
    },
    /// Open the TUI dashboard.
    Ui,
    /// Manage git hooks (install / run / uninstall).
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Manage agent instructions and skills sourced from upstream repos.
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },
    /// Emit a shell completion script (bash / zsh / fish / elvish / powershell).
    Completions { shell: clap_complete::Shell },
    /// Interactive prompt helpers usable from any shell script
    /// (`keel lib ask`, `confirm`, `password`, `select`, `filter`).
    Lib {
        #[command(subcommand)]
        action: LibAction,
    },
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
    /// Self-update the keel binary from the latest GitHub release.
    Update {
        /// Re-download and replace even if already on the latest version.
        #[arg(long)]
        force: bool,
        /// Include pre-releases when looking for the latest version.
        #[arg(long)]
        prerelease: bool,
    },
    /// Drop into an interactive shell. Defaults to the project's
    /// devcontainer when configured; pass `--service <name>` to enter
    /// a compose service instead.
    Shell {
        /// Open a shell inside the named compose service (e.g.
        /// `keel shell --service app`) instead of the devcontainer.
        /// Works even when `[devcontainer] enabled = false`.
        #[arg(long, value_name = "NAME")]
        service: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Install keel-managed git hook shims.
    Install {
        /// Stages to install (default: pre-commit).
        #[arg(long, value_delimiter = ',')]
        stages: Vec<String>,
    },
    /// Remove keel-managed git hook shims.
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
pub enum AgentsAction {
    /// Apply pinned upstream sources to the project tree.
    Install {
        /// Re-clone every source, ignoring the local cache.
        #[arg(long)]
        force: bool,
        /// Plan but don't write files or save state.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite keel-owned files that have been hand-edited
        /// since the last apply.
        #[arg(long)]
        force_overwrite_drift: bool,
    },
    /// Re-resolve revisions and re-apply. Floating refs (anything
    /// that isn't a SHA or semver tag) auto-refetch.
    Update {
        /// Limit the update to one or more named sources. May repeat.
        #[arg(long = "source", value_name = "NAME")]
        sources: Vec<String>,
        /// Re-clone every selected source, ignoring the local cache.
        #[arg(long)]
        force: bool,
        /// Plan but don't write files or save state.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite keel-owned files that have been hand-edited
        /// since the last apply.
        #[arg(long)]
        force_overwrite_drift: bool,
    },
    /// Show pinned-rev + drift status. Read-only — does not touch the
    /// upstream cache. Exits non-zero on drift / missing files when
    /// `--strict` is set.
    Status {
        #[arg(long)]
        strict: bool,
    },
    /// Print the actions a fresh apply would take, without touching
    /// any files.
    Diff,
}

#[derive(Debug, Subcommand)]
pub enum LibAction {
    /// Ask the user for a single-line text input. Prints answer to stdout.
    Ask {
        prompt: String,
        #[arg(long)]
        default: Option<String>,
    },
    /// Ask a yes/no question. Exits 0 on yes, 1 on no.
    Confirm {
        prompt: String,
        /// Default when the user just hits Enter (or when stdin is non-tty).
        #[arg(long, value_parser = parse_yes_no)]
        default: Option<bool>,
    },
    /// Ask for a password (no echo). Prints answer to stdout.
    Password { prompt: String },
    /// Pick from a list. Use `--multi` for multi-select.
    Select {
        prompt: String,
        /// Choices given as positional arguments. Ignored when `--from`
        /// is set.
        choices: Vec<String>,
        /// Allow selecting multiple items; one selection per output line.
        #[arg(long)]
        multi: bool,
        /// Default-selected index (single-select mode only).
        #[arg(long)]
        default: Option<usize>,
        /// Read choices from a file, or `-` to read from stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Fuzzy-filter picker. Same I/O contract as single-select.
    Filter {
        prompt: String,
        /// Choices as positional args (ignored when `--from` is set).
        choices: Vec<String>,
        /// Read choices from a file, or `-` to read from stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
}

fn parse_yes_no(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" | "1" => Ok(true),
        "n" | "no" | "false" | "0" => Ok(false),
        other => Err(format!("expected yes/no, got `{other}`")),
    }
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
        /// Write to .keel/local.toml (per-developer) instead of keel.toml.
        #[arg(long)]
        local: bool,
    },
}

pub async fn run(cli: Cli) -> Result<()> {
    init_tracing();

    // `init` and `completions` run before config load — `init` writes
    // the config that doesn't exist yet; `completions` should never fail
    // on a broken keel.toml because users may regenerate completions
    // *while fixing* such a file.
    if let Some(Command::Init { template }) = cli.command {
        let project_root = locate_project_root(cli.project.as_deref())?;
        return commands::init::run(&project_root, template);
    }
    if let Some(Command::Completions { shell }) = cli.command {
        return commands::completions::run(shell);
    }
    if let Some(Command::Lib { action }) = cli.command {
        let code = run_lib_action(action)?;
        std::process::exit(code);
    }
    // `update` runs without project context: a freshly-installed keel
    // anywhere on $PATH must be able to upgrade itself.
    if let Some(Command::Update { force, prerelease }) = cli.command {
        return commands::update::run(force, prerelease).await;
    }

    let project_root = locate_project_root(cli.project.as_deref())?;

    // Bootstrap pass: load keel.toml + local.toml so we can detect
    // the worktree identity using the user's `[worktrees]` settings
    // (modulus / seed / pinned assignments). Then load again with the
    // slug applied, so per-worktree overlays at
    // `.keel/worktrees/<slug>.toml` take effect.
    let bootstrap_cfg = keel_config::load_project_with_slug(&project_root, None)
        .with_context(|| format!("load project at {}", project_root.display()))?;
    let identity = keel_runtime::Identity::detect(&project_root, &bootstrap_cfg).await;
    let slug_for_overlay = if identity.is_isolated() {
        Some(identity.slug.as_str())
    } else {
        None
    };
    let config = keel_config::load_project_with_slug(&project_root, slug_for_overlay)
        .with_context(|| format!("load project at {}", project_root.display()))?;
    let cfg_arc = Arc::new(config);

    // Auto-write resolved env to `[worktrees] dotenv` (no-op when the
    // field is unset). Idempotent: only writes when the contents would
    // change, so file watchers / git status stay quiet on repeat
    // invocations. We do this *before* dispatch so any subcommand that
    // shells out (recipes, compose passthrough, in-container exec)
    // sees the freshly materialised file.
    commands::env::auto_write_if_configured(&cfg_arc, &project_root)
        .await
        .context("auto-write [worktrees] dotenv")?;

    if let Some(sub) = cli.command {
        return match sub {
            Command::List => cmd_list(&cfg_arc),
            Command::Which { name } => cmd_which(&cfg_arc, &name),
            Command::Env { write } => commands::env::run(&cfg_arc, &project_root, write).await,
            Command::Doctor => {
                let code = commands::doctor::run(&cfg_arc, &project_root).await?;
                std::process::exit(code);
            }
            Command::Init { .. } => unreachable!("handled above"),
            Command::Completions { .. } => unreachable!("handled above"),
            Command::Lib { .. } => unreachable!("handled above"),
            Command::Update { .. } => unreachable!("handled above"),
            Command::Ui => run_tui(Arc::clone(&cfg_arc), &project_root, &identity).await,
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
                    commands::hooks::install(&cfg_arc, &project_root, &stages).await
                }
                HooksAction::Uninstall { stages } => {
                    commands::hooks::uninstall(&project_root, &stages).await
                }
                HooksAction::Run { stage } => {
                    let code = commands::hooks::run(&cfg_arc, &project_root, &stage).await?;
                    std::process::exit(code);
                }
            },
            Command::Agents { action } => match action {
                AgentsAction::Install {
                    force,
                    dry_run,
                    force_overwrite_drift,
                } => {
                    commands::agents::install(
                        &cfg_arc,
                        &project_root,
                        force,
                        dry_run,
                        force_overwrite_drift,
                    )
                    .await
                }
                AgentsAction::Update {
                    sources,
                    force,
                    dry_run,
                    force_overwrite_drift,
                } => {
                    commands::agents::update(
                        &cfg_arc,
                        &project_root,
                        sources,
                        force,
                        dry_run,
                        force_overwrite_drift,
                    )
                    .await
                }
                AgentsAction::Status { strict } => {
                    let code = commands::agents::status(&cfg_arc, &project_root, strict).await?;
                    std::process::exit(code);
                }
                AgentsAction::Diff => commands::agents::diff(&cfg_arc, &project_root).await,
            },
            Command::Shell { service } => {
                let code = commands::shell::run(
                    &cfg_arc,
                    &project_root,
                    &identity,
                    service.as_deref(),
                )
                .await?;
                std::process::exit(code);
            }
            Command::Install {
                step,
                resume,
                restart,
                dry_run,
                list,
                update_hooks,
            } => {
                let code = dispatch_install(
                    Arc::clone(&cfg_arc),
                    project_root.clone(),
                    commands::install::InstallArgs {
                        step,
                        resume,
                        restart,
                        dry_run,
                        list,
                        update_hooks,
                        assume_fresh: false,
                    },
                )
                .await?;
                std::process::exit(code);
            }
        };
    }

    if cli.args.is_empty() {
        return run_tui(Arc::clone(&cfg_arc), &project_root, &identity).await;
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

/// Dispatch a `keel lib <verb>` subcommand. Pure CLI — never touches
/// the project config; that's the whole point.
fn run_lib_action(action: LibAction) -> Result<i32> {
    match action {
        LibAction::Ask { prompt, default } => commands::lib::ask(&prompt, default.as_deref()),
        LibAction::Confirm { prompt, default } => commands::lib::confirm(&prompt, default),
        LibAction::Password { prompt } => commands::lib::password(&prompt),
        LibAction::Select {
            prompt,
            choices,
            multi,
            default,
            from,
        } => commands::lib::select(&prompt, choices, multi, default, from.as_deref()),
        LibAction::Filter {
            prompt,
            choices,
            from,
        } => commands::lib::filter(&prompt, choices, from.as_deref()),
    }
}

/// Build the install plan, write the auto-managed `.keel/.gitignore`,
/// handle the resume prompt when state is mid-flight, then hand off to
/// the install runner.
async fn dispatch_install(
    config: Arc<Config>,
    project_root: PathBuf,
    mut args: commands::install::InstallArgs,
) -> Result<i32> {
    let plan = commands::install::plan::resolve(&config, &project_root)?;

    // Refresh the gitignore on every invocation. Idempotent; cheap.
    ensure_keel_gitignore(&config, &project_root).context("update .keel/.gitignore")?;

    let bypass_prompt =
        args.resume || args.restart || args.dry_run || args.list || args.step.is_some();
    if !bypass_prompt
        && let Some(state) = commands::install::state::InstallState::load(&project_root)?
        && let Some(idx) = state.first_unresolved()
    {
        let name = plan
            .get(idx)
            .map(|s| s.name.as_str())
            .unwrap_or("<unknown>");
        let prompt = format!("Previous install stopped at `{name}`. Resume from there? [Y/n]");
        // Use dialoguer for the prompt, but only when stdin is a tty —
        // otherwise act as if the user said "yes" (CI runs and piped
        // invocations want non-interactive resume by default).
        let want_resume = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(true)
                .interact()
                .unwrap_or(true)
        } else {
            true
        };
        if want_resume {
            args.resume = true;
        } else {
            args.assume_fresh = true;
        }
    }

    let backend = build_backend(&config).await?;
    commands::install::run(config, project_root, backend, plan, args).await
}

/// Write the project-managed `.keel/.gitignore` block. Path is
/// configurable via `[install].gitignore` (default `.keel/.gitignore`).
fn ensure_keel_gitignore(config: &Config, project_root: &Path) -> Result<()> {
    let rel = &config.install.gitignore;
    let p = Path::new(rel);
    let path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_root.join(p)
    };
    // The block lives inside `.keel/` by default, so the ignore
    // patterns are relative to that directory. Users moving the file
    // elsewhere are responsible for prefixing the patterns themselves.
    let body = "local.toml\nworktrees/\ncache/\ninstall.state.json\nagents.state.json\n";
    keel_config::managed_block::write(&path, body)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Build a fully-configured Executor for CLI dispatch: detects the
/// backend, attaches the pre-resolved worktree identity, and applies an
/// optional profile.
async fn build_executor(
    config: &Arc<Config>,
    project_root: &Path,
    identity: &keel_runtime::Identity,
    profile: Option<&str>,
) -> Result<Executor> {
    let backend = build_backend(config).await?;
    let mut executor =
        Executor::new(backend, Arc::clone(config), project_root).with_identity(identity.clone());
    if let Some(dc) = build_devcontainer(config, project_root, identity)? {
        executor = executor.with_devcontainer(dc);
    }
    if let Some(p) = profile {
        executor = executor.with_profile(p);
    }
    Ok(executor)
}

/// Construct an [`Arc<DevcontainerBackend>`] when devcontainer support
/// is opted in and a `devcontainer.json` is reachable. Returns
/// `Ok(None)` when disabled. Returns an `Err` when enabled but the
/// config is missing or malformed — opting in and silently degrading
/// would be more confusing than failing loudly.
pub(crate) fn build_devcontainer(
    config: &Config,
    project_root: &Path,
    identity: &keel_runtime::Identity,
) -> Result<Option<Arc<DevcontainerBackend>>> {
    if !config.devcontainer.enabled {
        return Ok(None);
    }
    let override_path = config.devcontainer.path.as_deref();
    let spec_path = DevcontainerSpec::discover(project_root, override_path)
        .context("locate devcontainer.json (devcontainer.enabled = true)")?;
    let spec = DevcontainerSpec::load(&spec_path)
        .with_context(|| format!("parse {}", spec_path.display()))?;
    let project_slug = project_slug_from_root(project_root);
    let dc_identity = DevcontainerIdentity {
        project_root: project_root.to_path_buf(),
        project_slug,
        worktree_slug: identity.slug.clone(),
    };
    Ok(Some(Arc::new(DevcontainerBackend::new(spec, dc_identity))))
}

/// Slug derived from the project-root basename. Used as the project
/// component of the devcontainer's deterministic container name when
/// `[project].name` isn't set (and even when it is, the basename is
/// the stable per-repo identifier — `[project].name` is descriptive).
fn project_slug_from_root(project_root: &Path) -> String {
    let basename = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    keel_runtime::slugify(basename)
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
        Resolution::Recipe(_) => println!("{name} → recipe in keel.toml"),
        Resolution::Script(_) => println!("{name} → script in .keel/commands/"),
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

/// Build a single `Arc<dyn Backend>` that combines the configured
/// container backend with any `services.custom` / `services.systemd`
/// declarations. The result is always a `ServiceRegistry`, even when
/// only compose is configured — registry-of-one is the uniform
/// shape, no special-case at the call sites.
async fn build_backend(config: &Config) -> Result<Arc<dyn Backend>> {
    use keel_config::model::Backend as B;

    // Container slot (compose / none / future docker / podman).
    let container: Option<Arc<dyn Backend>> = match config.containers.backend {
        B::None => None,
        B::Compose => Some(Arc::new(
            ComposeBackend::detect()
                .await
                .context("detect compose backend")?,
        )),
        B::Docker | B::Podman => anyhow::bail!(
            "backend `{:?}` is configured but not yet implemented; use `compose` or `none`",
            config.containers.backend
        ),
    };

    // Custom slot (services.custom + services.systemd, translated into
    // CustomEntry values by keel-runtime).
    let mut entries: Vec<keel_container::custom::CustomEntry> =
        Vec::with_capacity(config.services.custom.len() + config.services.systemd.len());
    for svc in &config.services.custom {
        entries.push(keel_runtime::services::from_custom(svc));
    }
    for svc in &config.services.systemd {
        entries.push(keel_runtime::services::from_systemd(svc));
    }
    let custom = if entries.is_empty() {
        None
    } else {
        Some(keel_container::custom::CustomBackend::new(entries))
    };

    Ok(Arc::new(keel_container::registry::ServiceRegistry::new(
        container, custom,
    )))
}

async fn run_tui(
    initial_config: Arc<Config>,
    initial_root: &Path,
    initial_identity: &keel_runtime::Identity,
) -> Result<()> {
    // Outer loop: each iteration is one TUI session. The user can
    // switch worktrees from inside the TUI (`W` modal) → we drop
    // out of `keel_tui::run`, rebuild config / backend / executor
    // against the new root, and re-enter. `Quit` ends the loop.
    // The active view is carried over so a hot-reload from the
    // Terminals or Diff view lands in the same view in the new
    // worktree. The identity's BaseRef label feeds the top bar's
    // branch slot — re-detected after every hot-reload.
    let mut current_root = initial_root.to_path_buf();
    let mut current_config = initial_config;
    let mut current_branch = branch_label(initial_identity);
    let mut next_view = keel_tui::View::ControlCenter;
    loop {
        let backend: Arc<dyn Backend> = match build_backend(&current_config).await {
            Ok(b) => b,
            Err(_) => Arc::new(keel_container::registry::ServiceRegistry::new(
                Some(Arc::new(keel_container::null::NullBackend)),
                None,
            )),
        };
        // Re-detect identity per loop iteration — worktree switches
        // change which slug the devcontainer container_name embeds.
        let identity = keel_runtime::Identity::detect(&current_root, &current_config).await;
        let devcontainer = build_devcontainer(&current_config, &current_root, &identity)
            .context("build devcontainer backend")?;
        let mut executor = keel_runtime::Executor::new(
            Arc::clone(&backend),
            Arc::clone(&current_config),
            &current_root,
        )
        .with_identity(identity.clone());
        if let Some(dc) = &devcontainer {
            executor = executor.with_devcontainer(Arc::clone(dc));
        }
        let outcome = keel_tui::run(
            Arc::clone(&current_config),
            executor,
            backend,
            devcontainer,
            &current_root,
            next_view,
            current_branch.clone(),
        )
        .await
        .context("run TUI")?;
        match outcome {
            keel_tui::DriveOutcome::Quit => return Ok(()),
            keel_tui::DriveOutcome::SwitchWorktree { path, view } => {
                // Reload config from the new root. Slug detection
                // happens during config load via the same bootstrap
                // pass `run` does on first start.
                let bootstrap_cfg = keel_config::load_project_with_slug(&path, None)
                    .with_context(|| format!("load project at {}", path.display()))?;
                let identity = keel_runtime::Identity::detect(&path, &bootstrap_cfg).await;
                let slug_for_overlay = if identity.is_isolated() {
                    Some(identity.slug.as_str())
                } else {
                    None
                };
                let new_cfg = keel_config::load_project_with_slug(&path, slug_for_overlay)
                    .with_context(|| format!("load project at {}", path.display()))?;
                current_root = path;
                current_config = Arc::new(new_cfg);
                current_branch = branch_label(&identity);
                next_view = view;
            }
        }
    }
}

/// Map a worktree Identity to the string we surface in the top bar:
/// the active branch, a short SHA for detached HEAD, or the linked
/// worktree's directory basename. None when we're not inside a git
/// repo at all — the renderer skips the slot entirely in that case.
fn branch_label(identity: &keel_runtime::Identity) -> Option<String> {
    match &identity.base_ref {
        keel_runtime::BaseRef::Branch(b) => Some(b.clone()),
        keel_runtime::BaseRef::DetachedSha(s) => Some(format!("det-{s}")),
        keel_runtime::BaseRef::WorktreeDir(d) => Some(d.clone()),
        keel_runtime::BaseRef::None => None,
    }
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
        if cur.join("keel.toml").exists() || cur.join(".keel").is_dir() {
            return Ok(cur);
        }
        if !cur.pop() {
            return Ok(std::env::current_dir()?);
        }
    }
}
