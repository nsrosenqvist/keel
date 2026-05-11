//! Background state worker.
//!
//! Owns the slow / chatty I/O the render loop used to drive itself
//! from its pre-render hooks. The render loop talks to the worker
//! via two channels:
//!
//! - **Commands** (`cmd_tx`): "do this thing." App-driven inputs:
//!   service list updated, user kicked an action that should refresh
//!   eagerly, shutdown.
//! - **Snapshots** (`snap_rx`): "this changed." Worker-driven outputs
//!   applied to App state by the render loop's pre-render drain.
//!
//! Currently handles periodic service status polling — the
//! pre-render-hook bottleneck the old `refresh_service_status` call
//! introduced (shelling out to compose ~200 ms per service every
//! tick). Other concerns (service tails, watcher ticks, diff bodies)
//! migrate in later phases.

use scaffl_container::{Backend, ServiceStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

/// How often the worker re-polls service status. Matches the
/// throttle the inline `refresh_service_status` used so behaviour is
/// observably the same — just off the render thread.
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Inputs the render loop sends to the worker.
#[derive(Debug)]
pub enum WorkerCommand {
    /// Replace the worker's notion of which services to poll. Sent
    /// at boot from the configured pane set, then again after
    /// `discover_services` lands new names so the auto-discovered
    /// rows start receiving status updates without a restart.
    SetServices(Vec<String>),
    /// Force a poll *now* instead of waiting for the next interval
    /// tick. Useful after compose `up` / `down` / `restart` so the
    /// indicators flip without a 2-second lag.
    PokeServiceStatus,
    /// End the worker. Sent in App::Drop (best-effort) so the task
    /// frees its backend Arc.
    Shutdown,
}

/// Outputs the worker pushes back. The render loop drains these via
/// `App::drain_worker_snapshots` between renders and folds them into
/// state.
#[derive(Debug)]
pub enum WorkerSnapshot {
    /// Updated status for one service.
    ServiceStatus { name: String, status: ServiceStatus },
}

/// Endpoints the App holds for talking to the worker.
pub struct WorkerHandle {
    pub cmd_tx: mpsc::UnboundedSender<WorkerCommand>,
    pub snap_rx: mpsc::UnboundedReceiver<WorkerSnapshot>,
}

/// Spawn the worker task with an initial service list. The task
/// runs detached; dropping the `WorkerHandle` closes `cmd_tx` and
/// the worker exits on its next select cycle.
pub fn spawn(backend: Arc<dyn Backend>, initial_services: Vec<String>) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (snap_tx, snap_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_worker(backend, initial_services, cmd_rx, snap_tx));
    WorkerHandle { cmd_tx, snap_rx }
}

async fn run_worker(
    backend: Arc<dyn Backend>,
    initial_services: Vec<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    snap_tx: mpsc::UnboundedSender<WorkerSnapshot>,
) {
    let mut services = initial_services;
    let mut tick = interval(STATUS_POLL_INTERVAL);
    // Burst-skip if we fell behind (e.g. backend stalled for 10s) so
    // we don't issue 5 catch-up polls in a row.
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // biased: process queued commands before the interval tick.
        // Without this, a SetServices that landed before the first
        // tick could still see the tick fire against the old service
        // list — racy and confusing in tests.
        tokio::select! {
            biased;
            cmd = cmd_rx.recv() => match cmd {
                Some(WorkerCommand::SetServices(s)) => {
                    services = s;
                }
                Some(WorkerCommand::PokeServiceStatus) => {
                    poll_all(&backend, &services, &snap_tx).await;
                }
                Some(WorkerCommand::Shutdown) | None => return,
            },
            _ = tick.tick() => {
                poll_all(&backend, &services, &snap_tx).await;
                // If the render loop has gone (TUI exited), stop.
                if snap_tx.is_closed() {
                    return;
                }
            }
        }
    }
}

async fn poll_all(
    backend: &Arc<dyn Backend>,
    services: &[String],
    snap_tx: &mpsc::UnboundedSender<WorkerSnapshot>,
) {
    for name in services {
        if let Ok(status) = backend.status(name).await
            && snap_tx
                .send(WorkerSnapshot::ServiceStatus {
                    name: name.clone(),
                    status,
                })
                .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scaffl_container::null::NullBackend;

    #[tokio::test]
    async fn poke_triggers_immediate_snapshot() {
        let backend: Arc<dyn Backend> = Arc::new(NullBackend);
        let mut handle = spawn(backend, vec!["api".into(), "db".into()]);
        handle
            .cmd_tx
            .send(WorkerCommand::PokeServiceStatus)
            .unwrap();
        // Two services → two snapshots; null backend reports Missing.
        let one = tokio::time::timeout(Duration::from_secs(1), handle.snap_rx.recv())
            .await
            .expect("first snapshot")
            .expect("channel open");
        let two = tokio::time::timeout(Duration::from_secs(1), handle.snap_rx.recv())
            .await
            .expect("second snapshot")
            .expect("channel open");
        let names: Vec<&str> = [&one, &two]
            .iter()
            .map(|s| match s {
                WorkerSnapshot::ServiceStatus { name, .. } => name.as_str(),
            })
            .collect();
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db"));
    }

    #[tokio::test]
    async fn set_services_replaces_the_poll_set() {
        let backend: Arc<dyn Backend> = Arc::new(NullBackend);
        let mut handle = spawn(backend, vec!["old".into()]);
        handle
            .cmd_tx
            .send(WorkerCommand::SetServices(vec!["new".into()]))
            .unwrap();
        handle
            .cmd_tx
            .send(WorkerCommand::PokeServiceStatus)
            .unwrap();
        let snap = tokio::time::timeout(Duration::from_secs(1), handle.snap_rx.recv())
            .await
            .expect("snapshot")
            .expect("channel open");
        match snap {
            WorkerSnapshot::ServiceStatus { name, .. } => assert_eq!(name, "new"),
        }
        // Drain any backlog (the very first auto-tick from spawn may
        // have already polled `old` before SetServices landed — that's
        // fine, we just assert the new set is what's polled after).
        // Subsequent poke should also report `new` only.
        handle
            .cmd_tx
            .send(WorkerCommand::PokeServiceStatus)
            .unwrap();
        let snap = tokio::time::timeout(Duration::from_secs(1), handle.snap_rx.recv())
            .await
            .expect("snapshot")
            .expect("channel open");
        match snap {
            WorkerSnapshot::ServiceStatus { name, .. } => assert_eq!(name, "new"),
        }
    }
}
