//! Recipe / script / lifecycle launches, abort, and per-tick run
//! drain + poll.
//!
//! Submodule of [`super::app`]; sees its parent's private fields
//! (`runs`, `executor`, `backend`, `lifecycle_run`, `items`,
//! `selected`, `config`, `services`) directly. Splitting these out
//! keeps `app.rs` focused on App's identity + view + modal state.

use super::{App, LaunchRejection, RunKey};
use crate::container::Backend;
use crate::tui::runner::RunState;
use crate::tui::views::control_center::state::ItemKind;
use std::sync::Arc;

impl App {
    pub fn try_launch_selected(&mut self) -> Result<(), LaunchRejection> {
        self.try_launch_selected_with_args(Vec::new())
    }

    /// Launch the currently selected item, forwarding `args` to it.
    /// Args reach the recipe / script via the same `forward_args =
    /// true` mechanism the CLI uses — the engine appends them to the
    /// argv after `shell_words` parses the run string.
    ///
    /// Returns [`LaunchRejection`] when the launch can't proceed.
    /// [`LaunchRejection::AlreadyRunning`] is the signal to open the
    /// kill-and-restart confirmation modal.
    pub fn try_launch_selected_with_args(
        &mut self,
        args: Vec<String>,
    ) -> Result<(), LaunchRejection> {
        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();

        // Per-row "is this already running?" gate replaces the old
        // single-slot `current_run` check. Different rows can run
        // concurrently; only the same row collides with itself.
        let key: RunKey = (item.kind, item.name.clone());
        if self.runs.get(&key).is_some_and(|r| !r.is_done()) {
            return Err(LaunchRejection::AlreadyRunning);
        }

        let executor = self
            .executor
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();

        match item.kind {
            ItemKind::Runtime => {
                return Err(LaunchRejection::NotRunnable(
                    "container row shows lifecycle output; use U / D / R / S".into(),
                ));
            }
            ItemKind::Service => {
                return Err(LaunchRejection::NotRunnable(
                    "service panes show logs; press enter to up the service".into(),
                ));
            }
            ItemKind::Watcher => {
                return Err(LaunchRejection::NotRunnable(
                    "watcher panes auto-run on file change".into(),
                ));
            }
            ItemKind::Recipe => {
                let Some(recipe) = self.config.commands.get(&item.name) else {
                    return Err(LaunchRejection::NotRunnable(format!(
                        "recipe `{}` vanished from config",
                        item.name
                    )));
                };
                if recipe.service.is_some() {
                    return Err(LaunchRejection::NotRunnable(
                        "in-container recipes from the TUI are deferred — run from the CLI".into(),
                    ));
                }
            }
            ItemKind::Script => {
                let Some(script) = self.config.scripts.get(&item.name) else {
                    return Err(LaunchRejection::NotRunnable(format!(
                        "script `{}` vanished from config",
                        item.name
                    )));
                };
                if script.service.is_some() {
                    return Err(LaunchRejection::NotRunnable(
                        "in-container scripts are deferred".into(),
                    ));
                }
            }
        }

        let run = RunState::spawn(&executor, item.name, args);
        self.runs.insert(key, run);
        self.clear_flash();
        Ok(())
    }

    /// Force-relaunch the selected row even if a previous run is in
    /// flight: aborts the existing one, drops its buffer, and spawns
    /// a fresh `RunState`. Called by the kill-and-restart confirmation
    /// path after the user says yes.
    pub fn force_relaunch_selected(&mut self) -> Result<(), LaunchRejection> {
        let item = self
            .selected_item()
            .ok_or_else(|| LaunchRejection::NotRunnable("no selection".into()))?
            .clone();
        let key: RunKey = (item.kind, item.name.clone());
        if let Some(run) = self.runs.get_mut(&key)
            && !run.is_done()
        {
            run.abort();
        }
        // Drop the old entry so the buffer starts fresh; try_launch_selected
        // would skip the spawn otherwise.
        self.runs.remove(&key);
        self.try_launch_selected()
    }

    /// Drain output for every active run plus the lifecycle slot.
    /// Cheap when nothing has arrived; called on every pre-render
    /// hook so navigation reveals up-to-date buffers.
    pub fn drain_runs(&mut self) {
        for run in self.runs.values_mut() {
            run.drain();
        }
        if let Some(run) = self.lifecycle_run.as_mut() {
            run.drain();
        }
    }

    /// Abort the run on the selected row, if it's in flight. Returns
    /// true when an abort was issued. With per-row runs, `s` no
    /// longer needs to know "is *anything* running" — only "is the
    /// thing I'm focused on running."
    pub fn abort_selected_run(&mut self) -> bool {
        let Some(item) = self.selected_item().cloned() else {
            return false;
        };
        let key: RunKey = (item.kind, item.name);
        match self.runs.get_mut(&key) {
            Some(run) if !run.is_done() => {
                run.abort();
                true
            }
            _ => false,
        }
    }

    /// Abort the lifecycle run if one is in flight. Used by `s` /
    /// `S` when no per-row run is active — keeps the "stop the
    /// noisy thing" muscle memory working.
    pub fn abort_lifecycle_run(&mut self) -> bool {
        match self.lifecycle_run.as_mut() {
            Some(run) if !run.is_done() => {
                run.abort();
                true
            }
            _ => false,
        }
    }

    /// Run a backend lifecycle action (up / down / stop / restart)
    /// against the given services. An empty slice means "all services".
    /// Output streams into the lifecycle slot independent of which
    /// sidebar row is selected; the renderer overlays it as a split
    /// view when in flight.
    ///
    /// Returns `Err` if no backend is configured or another lifecycle
    /// action is already running. Per-row recipe / script runs do not
    /// block lifecycle actions.
    pub async fn run_service_action(
        &mut self,
        action: &'static str,
        services: &[&str],
    ) -> Result<(), LaunchRejection> {
        if self.lifecycle_run.as_ref().is_some_and(|r| !r.is_done()) {
            return Err(LaunchRejection::AlreadyRunning);
        }
        let backend: Arc<dyn Backend> = self
            .backend
            .as_ref()
            .ok_or(LaunchRejection::NoExecutor)?
            .clone();
        match backend.service_action(action, services).await {
            Ok(child) => {
                let label = if services.is_empty() {
                    format!("compose {action}")
                } else {
                    format!("compose {action} {}", services.join(" "))
                };
                self.lifecycle_run = Some(RunState::spawn_child(label, child));
                // Auto-jump to the container row so the user sees the
                // action's output immediately. They can navigate away
                // mid-stream; the buffer stays in the row until the
                // next lifecycle action replaces it.
                if let Some(idx) = self.items.iter().position(|i| i.kind == ItemKind::Runtime) {
                    self.selected = idx;
                }
                Ok(())
            }
            Err(e) => Err(LaunchRejection::NotRunnable(format!("{e}"))),
        }
    }

    /// Poll completion for every active run plus the lifecycle slot.
    /// Each `RunState` short-circuits on already-done. Pokes the
    /// worker when the lifecycle slot transitions to done so service
    /// indicators flip without waiting for the next 2-second tick.
    pub fn poll_runs(&mut self) {
        for run in self.runs.values_mut() {
            run.poll_completion();
        }
        if let Some(run) = self.lifecycle_run.as_mut() {
            let was_done = run.is_done();
            run.poll_completion();
            if !was_done && run.is_done() {
                self.poke_worker_status();
            }
        }
    }

    /// Drain output from every service pane and detect any tail
    /// process that exited non-zero (so the error message gets
    /// surfaced on the padded error path). Cheap when nothing has
    /// arrived; called on every pre-render hook.
    pub fn drain_services(&mut self) {
        for pane in self.services.values_mut() {
            pane.poll_tail();
        }
    }
}
