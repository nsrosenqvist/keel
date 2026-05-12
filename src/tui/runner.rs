//! Run-state machinery for the TUI.
//!
//! When a recipe is launched from the dashboard, we spawn it on a tokio
//! task with a [`ChannelSink`](crate::runtime::ChannelSink) feeding output
//! into a per-run buffer. [`RunState`] holds the moving parts: the
//! receiver, the join handle, the captured lines, and the (eventually
//! known) exit code.
//!
//! The TUI event loop drains output non-blocking on each tick and polls
//! the join handle for completion, so neither blocks rendering.

use crate::runtime::{Executor, OutputLine, OutputStream, RuntimeError};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

/// How many output lines we keep per run. Older lines age out as new ones
/// arrive. 4 KiB lines × this cap is the worst-case memory bound.
pub const OUTPUT_BUFFER_CAP: usize = 2_000;

/// Snapshot of a captured line for rendering. Cheap to clone; we never
/// reach into [`OutputLine`] directly from the UI module.
#[derive(Debug, Clone)]
pub struct CapturedLine {
    pub stream: OutputStream,
    pub text: String,
}

impl From<OutputLine> for CapturedLine {
    fn from(o: OutputLine) -> Self {
        Self {
            stream: o.stream,
            text: o.line,
        }
    }
}

/// Lifecycle of a single run.
pub struct RunState {
    pub name: String,
    pub started_at: Instant,
    /// Set the moment a run reaches a terminal state (exit_code or
    /// error populated). Used for the title's duration suffix on
    /// completed runs — `started_at.elapsed()` would keep climbing
    /// after completion.
    pub finished_at: Option<Instant>,
    pub completion: Option<JoinHandle<Result<i32, RuntimeError>>>,
    pub output_rx: UnboundedReceiver<OutputLine>,
    pub buffer: VecDeque<CapturedLine>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl RunState {
    /// Wrap an arbitrary tokio [`Child`] (with piped stdio) as a
    /// [`RunState`]. Used for service-action runs (`compose up`, etc.)
    /// where there's no recipe to dispatch — the TUI gets a Child
    /// straight from [`crate::container::Backend::service_action`] and
    /// hands it off here for output capture and lifecycle tracking.
    ///
    /// The child must already have stdout / stderr piped (the spawning
    /// backend method takes care of that). `kill_on_drop` should also
    /// be set on the spawning Command so abort() actually kills the
    /// process.
    pub fn spawn_child(label: impl Into<String>, mut child: Child) -> Self {
        let (tx, output_rx) = tokio::sync::mpsc::unbounded_channel::<OutputLine>();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let tx_out = tx.clone();
        if let Some(s) = stdout {
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
            });
        }
        if let Some(s) = stderr {
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
            });
        }

        let completion = tokio::spawn(async move {
            child
                .wait()
                .await
                .map(|s| s.code().unwrap_or(-1))
                .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))
        });

        Self {
            name: label.into(),
            started_at: Instant::now(),
            finished_at: None,
            completion: Some(completion),
            output_rx,
            buffer: VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            exit_code: None,
            error: None,
        }
    }

    /// Spawn `name` through `executor` with a channel-backed sink.
    pub fn spawn(executor: &Executor, name: impl Into<String>, args: Vec<String>) -> Self {
        let (sink, output_rx) = crate::runtime::ChannelSink::new_pair();
        let owned_name: String = name.into();
        let task_name = owned_name.clone();
        let exec = executor.with_sink(Arc::new(sink));
        let completion = tokio::spawn(async move { exec.run_recipe(&task_name, &args).await });

        Self {
            name: owned_name,
            started_at: Instant::now(),
            finished_at: None,
            completion: Some(completion),
            output_rx,
            buffer: VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            exit_code: None,
            error: None,
        }
    }

    /// Drain available output non-blocking. Returns the number of lines
    /// pulled this tick — useful as a "did anything change" signal.
    pub fn drain(&mut self) -> usize {
        let mut drained = 0;
        while let Ok(line) = self.output_rx.try_recv() {
            push_capped(&mut self.buffer, line.into());
            drained += 1;
        }
        drained
    }

    /// Collect the result if the task has finished. No-op if still
    /// running or already done. Synchronous: only checks completion
    /// state and pulls the result via `now_or_never` (safe because we
    /// gated on `is_finished`).
    pub fn poll_completion(&mut self) {
        if self.is_done() {
            return;
        }
        let Some(handle) = self.completion.as_mut() else {
            return;
        };
        if !handle.is_finished() {
            return;
        }
        let handle = self.completion.take().expect("checked above");
        use futures::FutureExt;
        let result = handle
            .now_or_never()
            .expect("is_finished returned true above");
        match result {
            Ok(Ok(code)) => self.exit_code = Some(code),
            Ok(Err(e)) => self.error = Some(format!("{e}")),
            Err(e) => self.error = Some(format!("task panicked: {e}")),
        }
        self.finished_at = Some(Instant::now());
    }

    pub fn is_done(&self) -> bool {
        self.exit_code.is_some() || self.error.is_some()
    }

    /// Abort the running task. Aborts the JoinHandle, dropping the
    /// underlying child process; the executor sets `kill_on_drop` so
    /// the OS kills it. Records "aborted" in `error` for the title.
    /// No-op when the run is already done.
    pub fn abort(&mut self) {
        if self.is_done() {
            return;
        }
        if let Some(handle) = self.completion.take() {
            handle.abort();
        }
        self.error = Some("aborted".into());
        self.finished_at = Some(Instant::now());
    }

    /// Total wall-clock duration of the run. While running this is
    /// `now - started_at` (climbs in real time); after completion it
    /// freezes at `finished_at - started_at`.
    pub fn duration(&self) -> std::time::Duration {
        match self.finished_at {
            Some(t) => t.saturating_duration_since(self.started_at),
            None => self.started_at.elapsed(),
        }
    }

    /// Status line for the output pane title.
    pub fn status_label(&self) -> String {
        if let Some(code) = self.exit_code {
            if code == 0 {
                format!("{} · ✓ exit 0", self.name)
            } else {
                format!("{} · ✗ exit {}", self.name, code)
            }
        } else if let Some(err) = &self.error {
            format!("{} · ! {}", self.name, err)
        } else {
            format!(
                "{} · running ({:.1}s)",
                self.name,
                self.started_at.elapsed().as_secs_f32()
            )
        }
    }
}

/// Push a captured line into a buffer, evicting the oldest entry once the
/// buffer reaches [`OUTPUT_BUFFER_CAP`].
///
/// Shared between [`RunState`] and the per-service pane buffers so both
/// follow the same memory bound.
pub(crate) fn push_capped(buf: &mut VecDeque<CapturedLine>, line: CapturedLine) {
    if buf.len() == OUTPUT_BUFFER_CAP {
        buf.pop_front();
    }
    buf.push_back(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_capped_evicts_oldest() {
        let mut buf = VecDeque::new();
        for i in 0..(OUTPUT_BUFFER_CAP + 5) {
            push_capped(
                &mut buf,
                CapturedLine {
                    stream: OutputStream::Stdout,
                    text: format!("line {i}"),
                },
            );
        }
        assert_eq!(buf.len(), OUTPUT_BUFFER_CAP);
        assert_eq!(buf.front().unwrap().text, "line 5");
    }
}
