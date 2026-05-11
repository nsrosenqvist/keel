//! Generic shell-driven service backend.
//!
//! Drives non-container services — anything you can describe with a
//! status / start / stop / restart command quartet plus an optional
//! log-tailing command. systemd, brew services, ad-hoc daemons, etc.
//!
//! The container crate intentionally does not depend on keel-config.
//! Higher layers (the registry in `keel-runtime`) translate
//! `services.custom` and `services.systemd` declarations into
//! [`CustomEntry`] values and hand them to [`CustomBackend::new`].

use crate::{Backend, BackendError, ExecOptions, ServiceStatus};
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// One service this backend knows about. The runtime supplies these;
/// the container crate doesn't reach into any config schema.
#[derive(Debug, Clone)]
pub struct CustomEntry {
    pub name: String,
    /// Exit code only — stdout / stderr ignored. 0 = `Running`,
    /// non-zero = `Stopped`. Caller may use anything that exits
    /// quickly: `pgrep -x foo`, `systemctl --user is-active foo`,
    /// `curl -fsS http://localhost/health`, etc.
    pub status_cmd: String,
    pub start_cmd: String,
    pub stop_cmd: String,
    /// Always resolved by the caller — when the user didn't supply a
    /// restart explicitly, the caller passes `<stop> && <start>`.
    pub restart_cmd: String,
    /// Long-running command whose stdio streams into the TUI's
    /// service pane. `None` = no log source for this service.
    pub logs_cmd: Option<String>,
}

#[derive(Debug)]
pub struct CustomBackend {
    entries: Vec<CustomEntry>,
    by_name: HashMap<String, usize>,
}

impl CustomBackend {
    pub fn new(entries: Vec<CustomEntry>) -> Self {
        let by_name = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect();
        Self { entries, by_name }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn knows(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Synchronous iteration of declared service names. Mirrors what
    /// the async [`Backend::list_services`] returns, but available
    /// without async context — the registry uses this at construction
    /// time to seed its routing table.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    fn lookup(&self, name: &str) -> Result<&CustomEntry, BackendError> {
        self.by_name
            .get(name)
            .map(|&i| &self.entries[i])
            .ok_or_else(|| BackendError::Reported(format!("unknown custom service `{name}`")))
    }

    fn cmd_for<'a>(entry: &'a CustomEntry, action: &str) -> Result<&'a str, BackendError> {
        Ok(match action {
            "up" | "start" => entry.start_cmd.as_str(),
            "stop" | "down" => entry.stop_cmd.as_str(),
            "restart" => entry.restart_cmd.as_str(),
            other => {
                return Err(BackendError::Reported(format!(
                    "unsupported service action `{other}` (expected up / down / stop / restart)"
                )));
            }
        })
    }

    /// Resolve the target list. Empty input means "every known
    /// service" — matches compose's project-wide semantics for
    /// `down` / `stop` / `restart` / `up`.
    fn resolve_targets<'a>(
        &'a self,
        services: &[&str],
    ) -> Result<Vec<&'a CustomEntry>, BackendError> {
        if services.is_empty() {
            return Ok(self.entries.iter().collect());
        }
        services.iter().map(|n| self.lookup(n)).collect()
    }

    /// Synthesise the shell program for a multi-service action.
    /// First non-zero rc wins as the overall exit; later commands
    /// still run so the user sees what each service did.
    fn build_action_script(action: &str, targets: &[&CustomEntry]) -> Result<String, BackendError> {
        let mut script = String::from("set +e\noverall=0\n");
        for entry in targets {
            let cmd = Self::cmd_for(entry, action)?;
            // Quote name + action into the banner so a service called
            // `it's-fine` doesn't break the script.
            script.push_str(&format!(
                "printf '== %s: %s ==\\n' {name} {action}\n{cmd}\nrc=$?\nif [ $rc -ne 0 ] && [ $overall -eq 0 ]; then overall=$rc; fi\n",
                name = shell_escape(&entry.name),
                action = shell_escape(action),
                cmd = cmd,
            ));
        }
        script.push_str("exit $overall\n");
        Ok(script)
    }
}

#[async_trait]
impl Backend for CustomBackend {
    fn name(&self) -> &'static str {
        "custom"
    }

    async fn status(&self, service: &str) -> Result<ServiceStatus, BackendError> {
        let entry = self.lookup(service)?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(&entry.status_cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .status()
            .await?;
        Ok(if status.success() {
            ServiceStatus::Running
        } else {
            ServiceStatus::Stopped
        })
    }

    async fn exec(
        &self,
        service: &str,
        _argv: &[&str],
        _opts: &ExecOptions,
    ) -> Result<i32, BackendError> {
        Err(BackendError::Reported(format!(
            "in = \"{service}\" requires a container backend; `{service}` is a custom service"
        )))
    }

    async fn passthrough(&self, _args: &[&str]) -> Result<i32, BackendError> {
        Err(BackendError::Reported(
            "compose passthrough is not available without a container backend".into(),
        ))
    }

    async fn list_services(&self) -> Result<Vec<String>, BackendError> {
        Ok(self.entries.iter().map(|e| e.name.clone()).collect())
    }

    async fn service_action(&self, action: &str, services: &[&str]) -> Result<Child, BackendError> {
        let targets = self.resolve_targets(services)?;
        if targets.is_empty() {
            return Err(BackendError::Reported("no custom services declared".into()));
        }
        let script = Self::build_action_script(action, &targets)?;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }

    async fn tail_logs(&self, service: &str) -> Result<Child, BackendError> {
        let entry = self.lookup(service)?;
        let logs = entry.logs_cmd.as_deref().ok_or_else(|| {
            BackendError::Reported(format!(
                "no `logs` command declared for custom service `{service}`"
            ))
        })?;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(logs);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        cmd.spawn().map_err(BackendError::Spawn)
    }
}

/// Single-quote a value for safe insertion in a `sh` script. Wraps in
/// `'...'` and escapes embedded single quotes via the standard
/// `'\''` dance. Used only for the per-service banner labels — the
/// user-supplied commands themselves are passed through verbatim
/// since they're already shell strings.
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: &str, start: &str, stop: &str) -> CustomEntry {
        CustomEntry {
            name: name.into(),
            status_cmd: status.into(),
            start_cmd: start.into(),
            stop_cmd: stop.into(),
            restart_cmd: format!("{stop} && {start}"),
            logs_cmd: None,
        }
    }

    #[tokio::test]
    async fn status_running_when_command_succeeds() {
        let backend = CustomBackend::new(vec![entry("a", "true", ":", ":")]);
        assert_eq!(backend.status("a").await.unwrap(), ServiceStatus::Running);
    }

    #[tokio::test]
    async fn status_stopped_when_command_fails() {
        let backend = CustomBackend::new(vec![entry("a", "false", ":", ":")]);
        assert_eq!(backend.status("a").await.unwrap(), ServiceStatus::Stopped);
    }

    #[tokio::test]
    async fn status_unknown_service_errors() {
        let backend = CustomBackend::new(vec![entry("a", "true", ":", ":")]);
        let err = backend.status("does-not-exist").await.unwrap_err();
        match err {
            BackendError::Reported(msg) => {
                assert!(msg.contains("unknown custom service"), "msg: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_services_returns_all_entries() {
        let backend = CustomBackend::new(vec![
            entry("alpha", "true", ":", ":"),
            entry("beta", "true", ":", ":"),
        ]);
        let mut listed = backend.list_services().await.unwrap();
        listed.sort();
        assert_eq!(listed, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn exec_errors_with_helpful_hint() {
        let backend = CustomBackend::new(vec![entry("a", "true", ":", ":")]);
        let err = backend
            .exec("a", &["echo", "hi"], &ExecOptions::default())
            .await
            .unwrap_err();
        match err {
            BackendError::Reported(msg) => {
                assert!(msg.contains("requires a container backend"), "msg: {msg}");
                assert!(msg.contains("custom service"), "msg: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn service_action_runs_and_succeeds_when_all_succeed() {
        let backend = CustomBackend::new(vec![
            entry("a", "true", "true", "true"),
            entry("b", "true", "true", "true"),
        ]);
        let mut child = backend.service_action("up", &[]).await.unwrap();
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(0));
    }

    #[tokio::test]
    async fn service_action_first_failure_propagates() {
        let entries = vec![
            entry("a", "true", "true", "true"),
            CustomEntry {
                name: "b".into(),
                status_cmd: "true".into(),
                start_cmd: "exit 7".into(),
                stop_cmd: "true".into(),
                restart_cmd: "true".into(),
                logs_cmd: None,
            },
            entry("c", "true", "true", "true"),
        ];
        let backend = CustomBackend::new(entries);
        let mut child = backend.service_action("up", &[]).await.unwrap();
        let status = child.wait().await.unwrap();
        // First non-zero wins; later steps still ran.
        assert_eq!(status.code(), Some(7));
    }

    #[tokio::test]
    async fn service_action_routes_single_service() {
        let backend = CustomBackend::new(vec![
            entry("a", "true", "true", "true"),
            CustomEntry {
                name: "b".into(),
                status_cmd: "true".into(),
                start_cmd: "exit 11".into(),
                stop_cmd: "true".into(),
                restart_cmd: "true".into(),
                logs_cmd: None,
            },
        ]);
        // Asking for just `a` must skip `b` entirely — exit 0.
        let mut child = backend.service_action("up", &["a"]).await.unwrap();
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(0));
    }

    #[tokio::test]
    async fn service_action_unknown_action_errors() {
        let backend = CustomBackend::new(vec![entry("a", "true", "true", "true")]);
        let err = backend.service_action("nope", &["a"]).await.unwrap_err();
        match err {
            BackendError::Reported(msg) => assert!(msg.contains("unsupported service action")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tail_logs_errors_when_no_command_declared() {
        let backend = CustomBackend::new(vec![entry("a", "true", ":", ":")]);
        let err = backend.tail_logs("a").await.unwrap_err();
        match err {
            BackendError::Reported(msg) => assert!(msg.contains("no `logs` command declared")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tail_logs_streams_when_command_present() {
        let backend = CustomBackend::new(vec![CustomEntry {
            name: "a".into(),
            status_cmd: "true".into(),
            start_cmd: ":".into(),
            stop_cmd: ":".into(),
            restart_cmd: ":".into(),
            logs_cmd: Some("printf 'one\\ntwo\\n'".into()),
        }]);
        let child = backend.tail_logs("a").await.unwrap();
        let out = child.wait_with_output().await.unwrap();
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(s.contains("one"));
        assert!(s.contains("two"));
    }

    #[test]
    fn shell_escape_handles_quotes_and_spaces() {
        assert_eq!(shell_escape("simple"), "'simple'");
        assert_eq!(shell_escape("with space"), "'with space'");
        assert_eq!(shell_escape("it's"), r"'it'\''s'");
    }
}
