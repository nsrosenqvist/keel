//! Per-service log panes for the TUI.
//!
//! When a project's `[[ui.pane]]` declares a service, scaffl spawns a
//! `compose logs -f` tail at startup and pipes its output into a
//! [`ServicePane`] buffer. The pane is identical in structure to a
//! [`crate::runner::RunState`]'s buffer so the renderer can treat them
//! the same way.
//!
//! Status (running / stopped / missing) is polled at most once every
//! [`STATUS_REFRESH`]; the result drives the indicator dot in the
//! sidebar.

use crate::runner::{CapturedLine, OUTPUT_BUFFER_CAP, push_capped};
use scaffl_container::{Backend, BackendError, ServiceStatus};
use scaffl_runtime::{OutputLine, OutputStream};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

const STATUS_REFRESH: Duration = Duration::from_secs(2);

/// State for a single service pane.
pub struct ServicePane {
    pub name: String,
    pub buffer: VecDeque<CapturedLine>,
    rx: Option<UnboundedReceiver<OutputLine>>,
    /// Holds the child so it's killed on drop (compose backend sets
    /// `kill_on_drop`). Public for diagnostics; do not poll directly.
    tail_child: Option<Child>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
    pub status: Option<ServiceStatus>,
    /// Last error from `Backend::tail_logs`, if any. Surfaces in the
    /// pane title so the user knows why it's empty.
    pub tail_error: Option<String>,
    status_checked: Option<Instant>,
}

impl ServicePane {
    pub fn new(name: String) -> Self {
        Self {
            name,
            buffer: VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            rx: None,
            tail_child: None,
            stdout_task: None,
            stderr_task: None,
            status: None,
            tail_error: None,
            status_checked: None,
        }
    }

    pub fn is_tailing(&self) -> bool {
        self.tail_child.is_some()
    }

    /// Spawn the tail process if it isn't already running. Errors from the
    /// backend are stored in [`Self::tail_error`] rather than propagated,
    /// so a single missing service doesn't bring down the whole TUI.
    pub async fn ensure_tailing(&mut self, backend: &Arc<dyn Backend>) {
        if self.tail_child.is_some() {
            return;
        }
        match backend.tail_logs(&self.name).await {
            Ok(child) => self.attach(child),
            Err(e) => {
                self.tail_error = Some(format_error(&e));
            }
        }
    }

    fn attach(&mut self, mut child: Child) {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OutputLine>();

        let tx_out = tx.clone();
        let stdout_task = stdout.map(|s| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx_out
                        .send(OutputLine {
                            stream: OutputStream::Stdout,
                            line,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        let stderr_task = stderr.map(|s| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx
                        .send(OutputLine {
                            stream: OutputStream::Stderr,
                            line,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });

        self.tail_child = Some(child);
        self.rx = Some(rx);
        self.stdout_task = stdout_task;
        self.stderr_task = stderr_task;
        self.tail_error = None;
    }

    /// Drain any output that arrived since the last call.
    pub fn drain(&mut self) -> usize {
        let Some(rx) = self.rx.as_mut() else {
            return 0;
        };
        let mut drained = 0;
        while let Ok(line) = rx.try_recv() {
            push_capped(&mut self.buffer, CapturedLine::from(line));
            drained += 1;
        }
        drained
    }

    /// Refresh the cached status if the last check is older than
    /// [`STATUS_REFRESH`].
    pub async fn refresh_status(&mut self, backend: &Arc<dyn Backend>) {
        let now = Instant::now();
        let stale = self
            .status_checked
            .is_none_or(|t| now.duration_since(t) >= STATUS_REFRESH);
        if !stale {
            return;
        }
        self.status_checked = Some(now);
        if let Ok(s) = backend.status(&self.name).await {
            self.status = Some(s);
        }
    }
}

fn format_error(e: &BackendError) -> String {
    match e {
        BackendError::Reported(msg) => msg.clone(),
        BackendError::BinaryNotFound(name) => format!("backend binary not found: {name}"),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pane_starts_empty() {
        let pane = ServicePane::new("app".into());
        assert_eq!(pane.name, "app");
        assert!(!pane.is_tailing());
        assert!(pane.buffer.is_empty());
        assert!(pane.tail_error.is_none());
    }

    #[test]
    fn drain_with_no_rx_returns_zero() {
        let mut pane = ServicePane::new("app".into());
        assert_eq!(pane.drain(), 0);
    }
}
