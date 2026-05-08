//! Run-state machinery for the TUI.
//!
//! When a recipe is launched from the dashboard, we spawn it on a tokio
//! task with a [`ChannelSink`](scaffl_runtime::ChannelSink) feeding output
//! into a per-run buffer. [`RunState`] holds the moving parts: the
//! receiver, the join handle, the captured lines, and the (eventually
//! known) exit code.
//!
//! The TUI event loop drains output non-blocking on each tick and polls
//! the join handle for completion, so neither blocks rendering.

use scaffl_runtime::{Executor, OutputLine, OutputStream, RuntimeError};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
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
    pub completion: Option<JoinHandle<Result<i32, RuntimeError>>>,
    pub output_rx: UnboundedReceiver<OutputLine>,
    pub buffer: VecDeque<CapturedLine>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl RunState {
    /// Spawn `name` through `executor` with a channel-backed sink.
    pub fn spawn(executor: &Executor, name: impl Into<String>, args: Vec<String>) -> Self {
        let (sink, output_rx) = scaffl_runtime::ChannelSink::new_pair();
        let owned_name: String = name.into();
        let task_name = owned_name.clone();
        let exec = executor.with_sink(Arc::new(sink));
        let completion = tokio::spawn(async move { exec.run_recipe(&task_name, &args).await });

        Self {
            name: owned_name,
            started_at: Instant::now(),
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

    /// Await completion if the task has finished. No-op if still running
    /// or already done.
    pub async fn poll_completion(&mut self) {
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
        match handle.await {
            Ok(Ok(code)) => self.exit_code = Some(code),
            Ok(Err(e)) => self.error = Some(format!("{e}")),
            Err(e) => self.error = Some(format!("task panicked: {e}")),
        }
    }

    pub fn is_done(&self) -> bool {
        self.exit_code.is_some() || self.error.is_some()
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
