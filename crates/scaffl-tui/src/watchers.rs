//! TUI watcher panes.
//!
//! Each `[[ui.pane]] type = "watcher"` becomes a long-lived state machine
//! that:
//!
//! 1. Watches the project root recursively via [`notify`] and filters
//!    events through a [`globset::GlobSet`] built from the pane's globs.
//! 2. Debounces a stream of incoming events into one re-run per quiet
//!    window (default 300 ms; configurable per pane).
//! 3. On debounce expiry, spawns the configured recipe through
//!    [`scaffl_runtime::Executor`] with a [`scaffl_runtime::ChannelSink`]
//!    feeding output into a per-pane ring buffer.
//!
//! Single-threaded ergonomics: the [`WatcherPane::tick`] method advances
//! the state machine non-blocking. The TUI calls it on every pre-render
//! hook. Notify itself runs on a `spawn_blocking` task per pane so its
//! sync mpsc doesn't pollute the async main loop.

use crate::runner::{CapturedLine, OUTPUT_BUFFER_CAP, push_capped};
use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use scaffl_runtime::{ChannelSink, Executor, OutputLine, RuntimeError};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tracing::warn;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("invalid glob `{pattern}`: {source}")]
    Glob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("notify failed: {0}")]
    Notify(#[from] notify::Error),
}

/// State of a watcher pane's run cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherState {
    /// No pending or running work.
    Idle,
    /// At least one event arrived; will fire when the debounce window
    /// elapses with no further events.
    Debouncing,
    /// A recipe is currently executing.
    Running,
}

pub struct WatcherPane {
    pub name: String,
    pub recipe: String,
    pub globs: Vec<String>,
    pub debounce: Duration,

    pub state: WatcherState,
    pub buffer: VecDeque<CapturedLine>,
    pub last_exit_code: Option<i32>,
    pub last_finished_at: Option<Instant>,
    pub last_run_started_at: Option<Instant>,

    rx_events: UnboundedReceiver<()>,
    rx_output: Option<UnboundedReceiver<OutputLine>>,
    completion: Option<JoinHandle<Result<i32, RuntimeError>>>,
    pending_until: Option<Instant>,
}

impl WatcherPane {
    /// Construct a pane and spawn its filesystem watcher. Returns
    /// [`WatcherError`] if the globs don't compile or notify fails to
    /// start watching the project root.
    pub fn spawn(
        name: String,
        recipe: String,
        globs: Vec<String>,
        debounce: Duration,
        project_root: &Path,
    ) -> Result<Self, WatcherError> {
        let glob_set = build_glob_set(&globs)?;
        let rx_events = spawn_notify(glob_set, project_root.to_path_buf())?;
        Ok(Self {
            name,
            recipe,
            globs,
            debounce,
            state: WatcherState::Idle,
            buffer: VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            last_exit_code: None,
            last_finished_at: None,
            last_run_started_at: None,
            rx_events,
            rx_output: None,
            completion: None,
            pending_until: None,
        })
    }

    /// Advance the state machine. Idempotent and non-blocking.
    pub async fn tick(&mut self, executor: &Executor) {
        // 1. Drain any filesystem events; arm the debounce timer.
        let mut had_event = false;
        while self.rx_events.try_recv().is_ok() {
            had_event = true;
        }
        if had_event {
            self.pending_until = Some(Instant::now() + self.debounce);
            if matches!(self.state, WatcherState::Idle) {
                self.state = WatcherState::Debouncing;
            }
        }

        // 2. If a run is in progress, drain its output and check completion.
        if let Some(rx) = self.rx_output.as_mut() {
            while let Ok(line) = rx.try_recv() {
                push_capped(&mut self.buffer, CapturedLine::from(line));
            }
        }
        if let Some(handle) = self.completion.as_mut() {
            if handle.is_finished() {
                let h = self.completion.take().expect("checked above");
                match h.await {
                    Ok(Ok(code)) => self.last_exit_code = Some(code),
                    Ok(Err(_)) => self.last_exit_code = Some(-1),
                    Err(_) => self.last_exit_code = Some(-1),
                }
                self.last_finished_at = Some(Instant::now());
                self.state = WatcherState::Idle;
                self.rx_output = None;
            }
        }

        // 3. If debounce window has elapsed and we're not currently
        // running, kick off a fresh run. New events during a run do *not*
        // queue another — we trade strict accuracy for sanity (mirrors
        // pre-commit / cargo-watch behaviour).
        if !matches!(self.state, WatcherState::Running)
            && self.pending_until.is_some_and(|t| Instant::now() >= t)
        {
            self.spawn_run(executor);
        }
    }

    fn spawn_run(&mut self, executor: &Executor) {
        let (sink, rx) = ChannelSink::new_pair();
        let exec = executor.with_sink(Arc::new(sink));
        let recipe = self.recipe.clone();
        let handle = tokio::spawn(async move { exec.run_recipe(&recipe, &[]).await });
        self.completion = Some(handle);
        self.rx_output = Some(rx);
        self.pending_until = None;
        self.buffer.clear();
        self.state = WatcherState::Running;
        self.last_run_started_at = Some(Instant::now());
    }

    /// Status text for the sidebar / pane title.
    pub fn status_label(&self) -> String {
        match self.state {
            WatcherState::Idle => match self.last_exit_code {
                None => format!("{} · idle", self.name),
                Some(0) => format!("{} · ✓ idle", self.name),
                Some(c) => format!("{} · ✗ {} · idle", self.name, c),
            },
            WatcherState::Debouncing => {
                let remaining = self
                    .pending_until
                    .and_then(|t| t.checked_duration_since(Instant::now()))
                    .unwrap_or_else(|| Duration::from_millis(0));
                format!("{} · cooldown ({:.1}s)", self.name, remaining.as_secs_f32())
            }
            WatcherState::Running => {
                let elapsed = self
                    .last_run_started_at
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                format!("{} · running ({:.1}s)", self.name, elapsed)
            }
        }
    }
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet, WatcherError> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|source| WatcherError::Glob {
            pattern: p.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| WatcherError::Glob {
        pattern: patterns.join(", "),
        source,
    })
}

fn spawn_notify(
    glob_set: GlobSet,
    project_root: PathBuf,
) -> Result<UnboundedReceiver<()>, WatcherError> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(sync_tx) {
            Ok(w) => w,
            Err(e) => {
                warn!("watcher pane: failed to create notify watcher: {e}");
                return;
            }
        };
        if let Err(e) = watcher.watch(&project_root, RecursiveMode::Recursive) {
            warn!(
                "watcher pane: failed to watch {}: {e}",
                project_root.display()
            );
            return;
        }
        for res in sync_rx {
            let Ok(event) = res else { continue };
            if !event_is_relevant(&event) {
                continue;
            }
            let matched = event
                .paths
                .iter()
                .any(|p| matches_glob_relative(&glob_set, p, &project_root));
            if matched && tx.send(()).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

fn event_is_relevant(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
}

fn matches_glob_relative(set: &GlobSet, path: &Path, project_root: &Path) -> bool {
    let rel = path.strip_prefix(project_root).unwrap_or(path);
    set.is_match(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn glob_set_matches_relative_paths() {
        let set = build_glob_set(&["src/**/*.rs".to_string()]).unwrap();
        assert!(set.is_match(Path::new("src/lib.rs")));
        assert!(set.is_match(Path::new("src/foo/bar.rs")));
        assert!(!set.is_match(Path::new("README.md")));
    }

    #[test]
    fn matches_glob_relative_strips_root() {
        let root = PathBuf::from("/proj");
        let set = build_glob_set(&["src/**".to_string()]).unwrap();
        assert!(matches_glob_relative(
            &set,
            Path::new("/proj/src/main.rs"),
            &root
        ));
        assert!(!matches_glob_relative(
            &set,
            Path::new("/proj/Cargo.toml"),
            &root
        ));
    }

    #[tokio::test]
    async fn pane_starts_in_idle_state() {
        let dir = TempDir::new().unwrap();
        let pane = WatcherPane::spawn(
            "test".into(),
            "echo".into(),
            vec!["**/*".into()],
            Duration::from_millis(300),
            dir.path(),
        )
        .unwrap();
        assert_eq!(pane.state, WatcherState::Idle);
        assert!(pane.last_exit_code.is_none());
        assert!(pane.buffer.is_empty());
    }
}
