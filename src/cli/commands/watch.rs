//! `croft watch <recipe>` — re-run a recipe when watched files change.
//!
//! Uses [`notify`] for filesystem events. A blocking watcher thread funnels
//! events into a tokio mpsc channel; the main loop debounces them and runs
//! the recipe through [`crate::runtime::Executor`] (inheriting stdio so the
//! user sees output in their terminal).
//!
//! Defaults are pragmatic rather than clever:
//! - Without `--paths`, the watcher recurses the project root and ignores
//!   `target`, `.git`, `node_modules`, and dotfiles.
//! - Debounce is 300 ms, batching rapid editor saves into a single re-run.

use crate::config::Config;
use crate::container::{Backend, compose::ComposeBackend};
use crate::runtime::Executor;
use anyhow::Result;
use notify::{Event, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_DEBOUNCE_MS: u64 = 300;
const IGNORED_SEGMENTS: &[&str] = &["target", ".git", "node_modules", ".croft"];

pub async fn run(
    config: Arc<Config>,
    project_root: &Path,
    recipe: String,
    args: Vec<String>,
    paths: Vec<PathBuf>,
    debounce_ms: Option<u64>,
) -> Result<()> {
    let backend = build_backend(&config).await;
    let executor = Executor::new(backend, Arc::clone(&config), project_root);

    let watch_paths = if paths.is_empty() {
        vec![project_root.to_path_buf()]
    } else {
        paths
    };
    let debounce = Duration::from_millis(debounce_ms.unwrap_or(DEFAULT_DEBOUNCE_MS));

    println!(
        "Watching {} (debounce {} ms). Initial run...",
        format_paths(&watch_paths),
        debounce.as_millis()
    );
    run_once(&executor, &recipe, &args).await?;

    let mut events = spawn_watcher(&watch_paths)?;
    let mut pending: Option<Instant> = None;

    loop {
        tokio::select! {
            biased;
            ev = events.recv() => {
                let Some(event) = ev else { return Ok(()); };
                if event_is_relevant(&event) {
                    pending = Some(Instant::now() + debounce);
                }
            }
            _ = sleep_until(pending) => {
                pending = None;
                println!("\nFiles changed; re-running `{recipe}`...");
                if let Err(e) = run_once(&executor, &recipe, &args).await {
                    eprintln!("error: {e:#}");
                }
            }
        }
    }
}

async fn run_once(executor: &Executor, recipe: &str, args: &[String]) -> Result<()> {
    let code = executor.run_recipe(recipe, args).await?;
    if code == 0 {
        println!("✓ exit 0");
    } else {
        println!("✗ exit {code}");
    }
    Ok(())
}

async fn sleep_until(target: Option<Instant>) {
    match target {
        Some(when) => tokio::time::sleep_until(when.into()).await,
        None => std::future::pending::<()>().await,
    }
}

/// Returns true when the event represents a real edit we should react to.
/// Filters out events on ignored path segments (target/, .git/, etc.) and
/// access-only events (which `notify` may emit on some platforms).
fn event_is_relevant(event: &Event) -> bool {
    use notify::EventKind;
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|p| !is_ignored(p))
}

fn is_ignored(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        IGNORED_SEGMENTS.iter().any(|ig| s == *ig)
    })
}

fn spawn_watcher(paths: &[PathBuf]) -> Result<tokio::sync::mpsc::UnboundedReceiver<Event>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let paths = paths.to_vec();
    tokio::task::spawn_blocking(move || {
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(sync_tx) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("watch: failed to create watcher: {e}");
                return;
            }
        };
        for path in &paths {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                eprintln!("watch: failed to watch {}: {e}", path.display());
                return;
            }
        }
        for res in sync_rx {
            match res {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    });
    Ok(rx)
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

async fn build_backend(config: &Config) -> Arc<dyn Backend> {
    use crate::config::model::Backend as B;
    match config.runtime.backend {
        B::Compose => match ComposeBackend::detect().await {
            Ok(b) => Arc::new(b),
            Err(_) => Arc::new(crate::container::null::NullBackend),
        },
        _ => Arc::new(crate::container::null::NullBackend),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ignored_segments_match() {
        assert!(is_ignored(&PathBuf::from("/proj/target/debug/x")));
        assert!(is_ignored(&PathBuf::from("/proj/.git/HEAD")));
        assert!(is_ignored(&PathBuf::from("/proj/node_modules/foo")));
        assert!(!is_ignored(&PathBuf::from("/proj/src/main.rs")));
    }
}
