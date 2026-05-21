//! Script-run orchestration.
//!
//! Mirrors [`super::recipe`] but for `.croft/commands/<name>`
//! scripts: the script body is piped over stdin to `<interpreter>
//! -s -- <args>` (interpreter derived from the shebang), with the
//! same in-service / in-devcontainer / on-host fan-out the recipe
//! path uses.

use super::{Executor, WorkspaceTarget};
use crate::config::ScriptCommand;
use crate::container::{Backend, ExecOptions, ServiceStatus};
use crate::runtime::error::RuntimeError;
use tokio::process::Command;

impl Executor {
    pub(crate) async fn execute_script(
        &self,
        script: &ScriptCommand,
        args: &[String],
    ) -> Result<i32, RuntimeError> {
        // Scripts get two extra env vars on top of the base resolution
        // chain: CROFT_PROJECT_DIR (worktree project root) and
        // CROFT_SCRIPT_DIR (the script file's parent directory).
        // They land *between* the base env and the script's own
        // `env = {...}`, so user overrides still win — but the
        // common case of "I just want to know where my script
        // lives" works without `dirname "$0"` boilerplate. Both
        // values are host-side paths even for in-container scripts;
        // ignore them inside the container if irrelevant.
        let project_dir = self.project_root.display().to_string();
        let script_dir = script
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| project_dir.clone());
        let env = self
            .cached_base_env()
            .await?
            .clone()
            .with_overrides([
                ("CROFT_PROJECT_DIR", project_dir.as_str()),
                ("CROFT_SCRIPT_DIR", script_dir.as_str()),
            ])
            .with_overrides(script.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        // In-container script: pipe the script body to
        // `<interpreter> -s -- <args>` over the backend's stdin-piped
        // exec. The shebang (if any) drives the interpreter choice;
        // default is `sh`.
        if let Some(service) = &script.service {
            let body = std::fs::read_to_string(&script.path)
                .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
            let interpreter = parse_shebang_interpreter(&body).unwrap_or("sh");

            // Service must be running before we exec into it.
            let status = self.backend.status(service).await?;
            if status != ServiceStatus::Running {
                return Err(RuntimeError::Backend(
                    crate::container::BackendError::ServiceUnavailable {
                        service: service.clone(),
                        status: format!("{status:?}").to_lowercase(),
                    },
                ));
            }

            let mut argv: Vec<&str> = vec![interpreter, "-s"];
            if script.forward_args && !args.is_empty() {
                argv.push("--");
                argv.extend(args.iter().map(String::as_str));
            }
            let opts = ExecOptions {
                tty: false, // forced off — stdin pipe + TTY are mutually exclusive
                env: env.project_only_map(),
                workdir: None,
            };
            if self.sink.capture() {
                let child = self
                    .backend
                    .spawn_exec_with_stdin(service, &argv, &opts)
                    .await?;
                return self.write_stdin_and_stream(child, &body).await;
            }
            return self
                .backend
                .exec_with_stdin(service, &argv, &opts, &body)
                .await
                .map_err(RuntimeError::from);
        }

        if let WorkspaceTarget::Devcontainer(dc) = &self.workspace {
            // Mirror the in-service script path: pipe the script body
            // to `<interpreter> -s -- <args>` inside the devcontainer.
            // The container needs to be up first; the backend handles
            // ensure_up on `exec_with_stdin`.
            let body = std::fs::read_to_string(&script.path)
                .map_err(|e| RuntimeError::Backend(crate::container::BackendError::Spawn(e)))?;
            let interpreter = parse_shebang_interpreter(&body).unwrap_or("sh");
            let mut argv: Vec<&str> = vec![interpreter, "-s"];
            if script.forward_args && !args.is_empty() {
                argv.push("--");
                argv.extend(args.iter().map(String::as_str));
            }
            let opts = ExecOptions {
                tty: false,
                env: env.project_only_map(),
                workdir: None,
            };
            if self.sink.capture() {
                let child = dc
                    .spawn_exec_with_stdin(dc.container_name(), &argv, &opts)
                    .await?;
                return self.write_stdin_and_stream(child, &body).await;
            }
            return dc
                .exec_with_stdin(dc.container_name(), &argv, &opts, &body)
                .await
                .map_err(RuntimeError::from);
        }

        let mut cmd = Command::new(&script.path);
        if script.forward_args {
            cmd.args(args);
        }
        cmd.current_dir(self.project_root.as_ref());
        env.apply_to(&mut cmd);
        self.spawn_host(cmd).await
    }
}

/// Read the shebang (if any) of a script body and return a plain
/// interpreter name suitable for use inside a container.
///
/// Recognises the common cases — `bash`, `zsh`, `sh` — by substring
/// match against the shebang line. Anything else (including `python`,
/// `node`, etc.) returns `None`; the caller will fall back to `sh`.
/// This is intentionally narrow: containers usually have at least
/// `sh` and often `bash`; everything else is the script author's
/// responsibility to ensure.
pub(crate) fn parse_shebang_interpreter(body: &str) -> Option<&'static str> {
    let first_line = body.lines().next()?;
    let trimmed = first_line.strip_prefix("#!")?;
    if trimmed.contains("bash") {
        Some("bash")
    } else if trimmed.contains("zsh") {
        Some("zsh")
    } else if trimmed.contains("/sh") || trimmed.ends_with(" sh") || trimmed.trim_end() == "sh" {
        Some("sh")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shebang_bash() {
        assert_eq!(
            parse_shebang_interpreter("#!/usr/bin/env bash\necho hi\n"),
            Some("bash")
        );
        assert_eq!(parse_shebang_interpreter("#!/bin/bash\n"), Some("bash"));
    }

    #[test]
    fn shebang_zsh() {
        assert_eq!(
            parse_shebang_interpreter("#!/usr/bin/env zsh\n"),
            Some("zsh")
        );
    }

    #[test]
    fn shebang_sh() {
        assert_eq!(parse_shebang_interpreter("#!/bin/sh\n"), Some("sh"));
        assert_eq!(parse_shebang_interpreter("#!/usr/bin/env sh\n"), Some("sh"));
    }

    #[test]
    fn shebang_unknown() {
        assert_eq!(parse_shebang_interpreter("#!/usr/bin/python3\n"), None);
        assert_eq!(parse_shebang_interpreter("echo no shebang\n"), None);
        assert_eq!(parse_shebang_interpreter(""), None);
    }
}
