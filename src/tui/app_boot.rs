//! Boot-time wiring and per-tick drains.
//!
//! Submodule of [`super::app`]; sees its parent's private fields
//! directly. Splitting these out keeps `app.rs` focused on App's
//! identity (fields + the common state mutations) — the
//! background-task plumbing (spawn the worker, kick off the
//! discovery / diff-preload / watcher-spawn tasks, drain their
//! channels, poke the worker for fresh polls) all lives here.

use super::{App, DiffPreload, WatcherSpawnResult};
use crate::config::model::UiPane;
use crate::tui::services::ServicePane;
use crate::tui::views::terminals::state::TmuxWindow;
use crate::tui::watchers::WatcherPane;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

impl App {
    /// Spawn the three boot tasks (service discovery, diff preload,
    /// watcher panes) plus the background state worker. Returns
    /// immediately; results land via the channels stored on
    /// `self.boot` and are folded into App state by
    /// [`Self::drain_boot_results`].
    ///
    /// The render loop can paint its first frame before any of these
    /// finish — boot time is no longer gated on a slow Docker daemon
    /// or a cold git cache.
    pub fn spawn_boot_tasks(&mut self, project_root: &Path) {
        // 0. Spawn the background state worker if we have a backend.
        // Seeded with whatever services the config already declared
        // so the first poll tick has work to do without waiting for
        // discovery to land. Also seeds the tmux session name so the
        // worker can poll #{window_bell_flag} for the Terminals view.
        if let Some(backend) = self.backend.as_ref().map(Arc::clone) {
            let initial = self.services.keys().cloned().collect();
            let handle = crate::tui::worker::spawn(backend, initial);
            let _ = handle
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::SetTmuxSession(Some(
                    self.terminals.session_name.clone(),
                )));
            self.worker = Some(handle);
        }

        // 1. Service discovery — one-shot.
        if let Some(backend) = self.backend.as_ref().map(Arc::clone) {
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                // Empty Vec on failure: caller treats discovery as
                // best-effort (the auto-discovered group just stays
                // empty). Matches the pre-refactor `discover_services`
                // behaviour where a `list_services` Err was a no-op.
                let names = backend.list_services().await.unwrap_or_default();
                let _ = tx.send(names);
            });
            self.boot.discover_rx = Some(rx);
        }

        // 2. Diff preload — one-shot.
        let project_root_owned = project_root.to_path_buf();
        let configured_base = self.config.diff.base.clone();
        let (diff_tx, diff_rx) = oneshot::channel();
        tokio::spawn(async move {
            let trunk =
                crate::runtime::detect_trunk(&project_root_owned, configured_base.as_deref()).await;
            let anchor = match trunk.as_deref() {
                Some(t) => crate::runtime::merge_base(&project_root_owned, t).await,
                None => None,
            };
            let branch = crate::tui::views::diff::git::current_branch(&project_root_owned).await;
            let anchor_short = anchor
                .as_deref()
                .map(|sha| sha.chars().take(7).collect::<String>());
            let lazygit_available = crate::tui::lazygit::is_available();
            let files = crate::tui::views::diff::git::load_diff_files(
                &project_root_owned,
                anchor.as_deref(),
            )
            .await;
            let _ = diff_tx.send(DiffPreload {
                trunk,
                anchor,
                anchor_short,
                branch,
                lazygit_available,
                files,
            });
        });
        self.boot.diff_rx = Some(diff_rx);

        // 3. Watcher panes — mpsc, one item per pane. Spawning is sync
        // (globset compile + notify init) so we hop to `spawn_blocking`;
        // each pane streams back independently so a slow pane (notify
        // init on a deep tree) doesn't hold up the others.
        let watcher_specs: Vec<(usize, String, Vec<String>, u64)> = self
            .config
            .ui
            .panes
            .iter()
            .enumerate()
            .filter_map(|(idx, pane)| match pane {
                UiPane::Watcher {
                    glob,
                    on_change,
                    debounce_ms,
                    ..
                } => Some((idx, on_change.clone(), glob.clone(), *debounce_ms)),
                _ => None,
            })
            .collect();
        if !watcher_specs.is_empty() {
            let (w_tx, w_rx) = mpsc::unbounded_channel();
            let project_root_for_watchers = project_root.to_path_buf();
            tokio::task::spawn_blocking(move || {
                // Replicate `unique_watcher_name` locally — we don't
                // hold a reference to App from this thread, so we
                // dedupe against names we've already emitted.
                let mut emitted: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for (idx, on_change, glob, debounce_ms) in watcher_specs {
                    let primary = format!("watch:{on_change}");
                    let name = if emitted.insert(primary.clone()) {
                        primary
                    } else {
                        let fallback = format!("watch:{on_change}:{idx}");
                        emitted.insert(fallback.clone());
                        fallback
                    };
                    let pane = WatcherPane::spawn(
                        name.clone(),
                        on_change,
                        glob,
                        Duration::from_millis(debounce_ms),
                        &project_root_for_watchers,
                    );
                    if w_tx.send(WatcherSpawnResult { name, pane }).is_err() {
                        // Receiver dropped — TUI exited mid-spawn.
                        return;
                    }
                }
            });
            self.boot.watcher_rx = Some(w_rx);
        }
    }

    /// Non-blocking drain of any boot-task results that have landed
    /// since the last call. Called once per loop iteration in the
    /// pre-render hooks so the sidebar / header fill in as soon as
    /// each task completes — no extra wake needed.
    pub fn drain_boot_results(&mut self) {
        // Service discovery.
        if let Some(rx) = self.boot.discover_rx.as_mut()
            && let Ok(names) = rx.try_recv()
        {
            self.boot.discover_rx = None;
            let mut added = false;
            for name in names {
                if !self.services.contains_key(&name) {
                    self.services
                        .insert(name.clone(), ServicePane::new(name.clone()));
                    added = true;
                }
            }
            if added {
                self.items = super::build_items_from(
                    &self.config,
                    &self.services,
                    &self.watchers,
                );
                // Refresh the worker's service set so the auto-
                // discovered rows start receiving status updates.
                if let Some(w) = self.worker.as_ref() {
                    let _ = w
                        .cmd_tx
                        .send(crate::tui::worker::WorkerCommand::SetServices(
                            self.services.keys().cloned().collect(),
                        ));
                }
            }
        }

        // Diff preload.
        if let Some(rx) = self.boot.diff_rx.as_mut()
            && let Ok(preload) = rx.try_recv()
        {
            self.boot.diff_rx = None;
            self.apply_diff_preload(preload);
        }

        // Watcher panes — drain everything that arrived this tick.
        if let Some(rx) = self.boot.watcher_rx.as_mut() {
            let mut added = false;
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(result) => match result.pane {
                        Ok(pane) => {
                            self.watchers.insert(result.name, pane);
                            added = true;
                        }
                        Err(e) => {
                            tracing::warn!("watcher pane `{}` failed to start: {e}", result.name);
                        }
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected {
                self.boot.watcher_rx = None;
            }
            if added {
                self.items = super::build_items_from(
                    &self.config,
                    &self.services,
                    &self.watchers,
                );
            }
        }
    }

    pub(crate) fn apply_diff_preload(&mut self, preload: DiffPreload) {
        self.diff.set_anchor(
            preload.trunk,
            preload.anchor,
            preload.branch,
            preload.anchor_short,
        );
        self.diff.set_lazygit_available(preload.lazygit_available);
        match preload.files {
            Ok(files) => self.diff.set_files(files),
            Err(msg) => self.diff.set_error(msg),
        }
    }

    /// Apply any worker snapshots that have arrived since the last
    /// call. Non-blocking — replaces the inline
    /// `refresh_service_status` await that used to shell out to
    /// compose for each service on every pre-render tick.
    pub fn drain_worker_snapshots(&mut self) {
        // Coalesce tmux-window snapshots: only the most recent
        // reflects the current state, and applying older ones first
        // would briefly flicker stale data on the next render.
        let mut latest_tmux: Option<Vec<TmuxWindow>> = None;
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        loop {
            match w.snap_rx.try_recv() {
                Ok(crate::tui::worker::WorkerSnapshot::ServiceStatus { name, status }) => {
                    if let Some(pane) = self.services.get_mut(&name) {
                        pane.status = Some(status);
                    }
                }
                Ok(crate::tui::worker::WorkerSnapshot::TmuxWindows(windows)) => {
                    latest_tmux = Some(windows);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.worker = None;
                    break;
                }
            }
        }
        if let Some(windows) = latest_tmux {
            self.terminals_set_windows(windows);
        }
    }

    /// Signal the worker that service-affecting state just changed
    /// (e.g. compose `up`/`down`/`restart`) so it polls immediately
    /// rather than waiting for the next interval tick. Best-effort —
    /// the next tick would still pick the change up within ~2 s.
    pub fn poke_worker_status(&self) {
        if let Some(w) = self.worker.as_ref() {
            let _ = w
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::PokeServiceStatus);
        }
    }

    /// Signal the worker to refresh the tmux window list now —
    /// after an attach return, kill, or view entry, so the row list
    /// (and bell flags) feel reactive instead of waiting for the
    /// 1 s tick.
    pub fn poke_worker_tmux_windows(&self) {
        if let Some(w) = self.worker.as_ref() {
            let _ = w
                .cmd_tx
                .send(crate::tui::worker::WorkerCommand::PokeTmuxWindows);
        }
    }

    /// Spawn tail processes for every service pane that doesn't
    /// already have one. Called once at startup; idempotent if
    /// called again.
    pub async fn spawn_service_tails(&mut self) {
        let Some(backend) = self.backend.as_ref().map(Arc::clone) else {
            return;
        };
        for pane in self.services.values_mut() {
            pane.ensure_tailing(&backend).await;
        }
    }

    /// Spawn watcher panes from `[[ui.pane]] type = "watcher"`. Spawn
    /// failures (bad globs, notify init failures) are logged and the
    /// pane is omitted; the rest of the dashboard still works.
    pub fn spawn_watcher_panes(&mut self, project_root: &Path) {
        let mut added = false;
        for (idx, pane) in self.config.ui.panes.iter().enumerate() {
            if let UiPane::Watcher {
                glob,
                on_change,
                debounce_ms,
                ..
            } = pane
            {
                let name = super::unique_watcher_name(on_change, idx, &self.watchers);
                match WatcherPane::spawn(
                    name.clone(),
                    on_change.clone(),
                    glob.clone(),
                    Duration::from_millis(*debounce_ms),
                    project_root,
                ) {
                    Ok(p) => {
                        self.watchers.insert(name, p);
                        added = true;
                    }
                    Err(e) => {
                        tracing::warn!("watcher pane `{on_change}` failed to start: {e}");
                    }
                }
            }
        }
        if added {
            self.items =
                super::build_items_from(&self.config, &self.services, &self.watchers);
        }
    }
}
