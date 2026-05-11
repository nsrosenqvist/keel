//! Output sinks — pluggable strategies for what happens to a recipe's
//! stdout/stderr.
//!
//! - [`InheritSink`] (the default) lets the child inherit the parent's
//!   stdio. No pipes, no overhead, output streams straight to the user's
//!   terminal. This is what the CLI wants in 99% of cases.
//! - [`ChannelSink`] pipes stdout/stderr and forwards line-by-line through
//!   an [`tokio::sync::mpsc`] channel. The TUI uses this to show recipe
//!   output in a dedicated pane while the rest of the dashboard stays
//!   responsive.
//!
//! The [`OutputSink`] trait is dyn-compatible and stored as
//! `Arc<dyn OutputSink>` on the [`Executor`](crate::runtime::Executor). Adding a
//! new strategy (e.g. tee-to-file, ring-buffer-to-disk-cache) is a matter
//! of implementing the trait.

use tokio::sync::mpsc;

/// Which standard stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// A single line of captured output.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub stream: OutputStream,
    pub line: String,
}

pub trait OutputSink: Send + Sync {
    /// Whether the sink wants the executor to pipe child stdio. When
    /// `false`, the child inherits the parent's stdio and `write_line` is
    /// never called.
    fn capture(&self) -> bool {
        true
    }

    /// Receive a single output line. Implementations should be fast — they
    /// run inside the line-reader task.
    fn write_line(&self, stream: OutputStream, line: &str);
}

/// Default sink: do nothing, child inherits stdio.
#[derive(Debug, Default, Clone, Copy)]
pub struct InheritSink;

impl OutputSink for InheritSink {
    fn capture(&self) -> bool {
        false
    }
    fn write_line(&self, _stream: OutputStream, _line: &str) {
        // unreachable when capture() returns false
    }
}

/// Channel-backed sink. Lines are forwarded as [`OutputLine`] values.
/// A dropped receiver is treated as cancellation: subsequent writes are
/// silently discarded so the executor doesn't error mid-stream.
#[derive(Debug, Clone)]
pub struct ChannelSink {
    tx: mpsc::UnboundedSender<OutputLine>,
}

impl ChannelSink {
    pub fn new(tx: mpsc::UnboundedSender<OutputLine>) -> Self {
        Self { tx }
    }

    /// Convenience constructor returning the receiver alongside the sink.
    pub fn new_pair() -> (Self, mpsc::UnboundedReceiver<OutputLine>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl OutputSink for ChannelSink {
    fn write_line(&self, stream: OutputStream, line: &str) {
        let _ = self.tx.send(OutputLine {
            stream,
            line: line.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_sink_does_not_capture() {
        let sink = InheritSink;
        assert!(!sink.capture());
    }

    #[test]
    fn channel_sink_forwards_lines() {
        let (sink, mut rx) = ChannelSink::new_pair();
        sink.write_line(OutputStream::Stdout, "hello");
        sink.write_line(OutputStream::Stderr, "boom");
        let a = rx.try_recv().unwrap();
        let b = rx.try_recv().unwrap();
        assert_eq!(a.stream, OutputStream::Stdout);
        assert_eq!(a.line, "hello");
        assert_eq!(b.stream, OutputStream::Stderr);
        assert_eq!(b.line, "boom");
    }

    #[test]
    fn channel_sink_drops_silently_when_receiver_gone() {
        let (sink, rx) = ChannelSink::new_pair();
        drop(rx);
        // Should not panic.
        sink.write_line(OutputStream::Stdout, "lost");
    }
}
