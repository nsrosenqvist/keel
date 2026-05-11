//! Per-service log panes for the TUI.
//!
//! When a project's `[[ui.pane]]` declares a service, scaffl spawns a
//! `compose logs -f` tail at startup and pipes its output into a
//! [`ServicePane`] buffer. The pane is identical in structure to a
//! [`crate::runner::RunState`]'s buffer so the renderer can treat them
//! the same way.
//!
//! Status (running / stopped / missing) is polled by the background
//! [`crate::worker`] on its own cadence; the worker pushes
//! `ServiceStatus` snapshots that the render loop folds into
//! `ServicePane::status` via `App::drain_worker_snapshots`.

use crate::runner::{CapturedLine, OUTPUT_BUFFER_CAP, push_capped};
use scaffl_container::{Backend, BackendError, ServiceStatus};
use scaffl_runtime::{OutputLine, OutputStream};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

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

    /// Check whether the tail child has exited. If it exited non-zero
    /// (typical "Cannot connect to the Docker daemon" / "no such
    /// service" cases) promote the captured buffer into [`tail_error`]
    /// so the renderer shows it on the padded error path instead of
    /// the flush log buffer.
    ///
    /// On success-exit (e.g. the user stopped the service via
    /// `compose stop`) we just drop the child reference so a fresh
    /// `up` will re-attach.
    pub fn poll_tail(&mut self) {
        // Drain pending lines first — there may be a few left over from
        // the child's stderr after it exited but before we noticed.
        self.drain();

        let Some(child) = self.tail_child.as_mut() else {
            return;
        };
        let exited = match child.try_wait() {
            Ok(Some(status)) => status,
            _ => return,
        };
        // Take a final pass at the buffer in case the line readers
        // raced the exit.
        self.drain();
        self.tail_child = None;

        if !exited.success() {
            // Use whatever the child managed to print as the error
            // payload. The renderer wraps this on the padded path.
            let combined: String = self
                .buffer
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<&str>>()
                .join("  ");
            self.tail_error = Some(if combined.is_empty() {
                let code = exited
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into());
                format!("tail exited {code}")
            } else {
                combined
            });
            self.buffer.clear();
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
