//! Host-process spawning + child-stream pumping.
//!
//! Shared between the host-exec path (where the executor owns the
//! `Command`) and the in-container stdin-pipe path (where the
//! backend hands back a spawned `Child` we need to write into and
//! pump). Centralising both here keeps the "stdout / stderr -> sink"
//! plumbing in one place.

use super::Executor;
use crate::runtime::error::RuntimeError;
use crate::runtime::sink::OutputStream;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

impl Executor {
    /// Run a [`Command`] on the host, honouring the configured
    /// [`crate::runtime::sink::OutputSink`]: pipe-and-stream when the
    /// sink wants capture, or inherit-and-await when it doesn't.
    pub(crate) async fn spawn_host(&self, mut cmd: Command) -> Result<i32, RuntimeError> {
        if !self.sink.capture() {
            let status = cmd
                .status()
                .await
                .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
            return Ok(status.code().unwrap_or(-1));
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        // When the consumer (TUI) drops the spawning task, the Child
        // is dropped, and kill_on_drop fires SIGKILL. Without this,
        // aborting the JoinHandle would leak the process.
        cmd.kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
        self.stream_child_to_sink(child).await
    }

    /// Write `body` into the child's piped stdin (closing it on EOF
    /// so `bash -s` / `sh -s` start) and stream the rest through the
    /// configured sink. Used for the in-container script exec path
    /// when the TUI sink wants line-by-line capture.
    pub(crate) async fn write_stdin_and_stream(
        &self,
        mut child: tokio::process::Child,
        body: &str,
    ) -> Result<i32, RuntimeError> {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin_handle) = child.stdin.take() {
            stdin_handle
                .write_all(body.as_bytes())
                .await
                .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
            drop(stdin_handle);
        }
        self.stream_child_to_sink(child).await
    }

    /// Pump a piped-stdio [`tokio::process::Child`] through the
    /// configured sink and await its exit code. Shared between host
    /// exec (where we own the spawn) and container exec (where the
    /// backend hands us the already-spawned [`tokio::process::Child`]).
    pub(crate) async fn stream_child_to_sink(
        &self,
        mut child: tokio::process::Child,
    ) -> Result<i32, RuntimeError> {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_task = stdout.map(|s| {
            let sink = Arc::clone(&self.sink);
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink.write_line(OutputStream::Stdout, &line);
                }
            })
        });
        let stderr_task = stderr.map(|s| {
            let sink = Arc::clone(&self.sink);
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink.write_line(OutputStream::Stderr, &line);
                }
            })
        });

        let status = child
            .wait()
            .await
            .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
        if let Some(t) = stdout_task {
            let _ = t.await;
        }
        if let Some(t) = stderr_task {
            let _ = t.await;
        }
        Ok(status.code().unwrap_or(-1))
    }
}
