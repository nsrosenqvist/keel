//! `scaffl doctor` — validate config and report.

use anyhow::Result;
use scaffl_config::Config;
use scaffl_container::{Backend, compose::ComposeBackend};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Fail,
}

impl Severity {
    fn marker(self) -> &'static str {
        match self {
            Severity::Ok => "[ OK ]",
            Severity::Warn => "[WARN]",
            Severity::Fail => "[FAIL]",
        }
    }
}

#[derive(Debug, Clone)]
struct Finding {
    severity: Severity,
    message: String,
}

impl Finding {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: message.into(),
        }
    }
    fn fail(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            message: message.into(),
        }
    }
}

/// Run all checks and print a report. Returns exit code 0 if no FAILs;
/// 1 otherwise. Warnings do not change the exit code.
pub async fn run(config: &Config, project_root: &Path) -> Result<i32> {
    let mut findings = Vec::new();
    findings.extend(check_backend(config).await);
    findings.extend(check_env_files(config, project_root));
    findings.extend(check_dependency_graph(config));
    findings.extend(check_service_hints(config));

    let mut had_fail = false;
    for f in &findings {
        if f.severity == Severity::Fail {
            had_fail = true;
        }
        println!("{} {}", f.severity.marker(), f.message);
    }

    let summary = format!(
        "{} checks: {} ok, {} warn, {} fail",
        findings.len(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Ok)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == Severity::Fail)
            .count(),
    );
    println!("\n{summary}");

    Ok(if had_fail { 1 } else { 0 })
}

async fn check_backend(config: &Config) -> Vec<Finding> {
    use scaffl_config::model::Backend as B;
    match config.runtime.backend {
        B::None => vec![Finding::ok(
            "backend: none (no container backend configured)",
        )],
        B::Compose => match ComposeBackend::detect().await {
            Ok(b) => vec![Finding::ok(format!("backend: {} detected", b.name()))],
            Err(_) => vec![Finding::fail(
                "backend: docker compose / docker-compose not found on PATH",
            )],
        },
        B::Docker | B::Podman => vec![Finding::warn(format!(
            "backend: `{:?}` is configured but only `compose` is implemented in this version",
            config.runtime.backend
        ))],
    }
}

fn check_env_files(config: &Config, project_root: &Path) -> Vec<Finding> {
    if config.env_files.files.is_empty() {
        return vec![Finding::ok("env_files: none declared")];
    }
    let mut out = Vec::new();
    for raw in &config.env_files.files {
        // We don't expand ${VAR} here — doctor is best-effort and a literal
        // path mismatch is still useful signal.
        let path = project_root.join(raw);
        if path.exists() {
            out.push(Finding::ok(format!("env file `{raw}` present")));
        } else {
            out.push(Finding::warn(format!(
                "env file `{raw}` not found (will be skipped at runtime)"
            )));
        }
    }
    out
}

fn check_dependency_graph(config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();
    let known: Vec<&String> = config
        .commands
        .keys()
        .chain(config.scripts.keys())
        .collect();

    let check_deps = |name: &str, deps: &[String], kind: &str| -> Vec<Finding> {
        let mut findings = Vec::new();
        for dep in deps {
            if !known.iter().any(|k| k.as_str() == dep) {
                findings.push(Finding::fail(format!(
                    "{kind} `{name}` depends on unknown `{dep}`"
                )));
            }
        }
        findings
    };

    for (name, recipe) in &config.commands {
        out.extend(check_deps(name, &recipe.needs, "recipe"));
    }
    for (name, script) in &config.scripts {
        out.extend(check_deps(name, &script.needs, "script"));
    }

    if out.is_empty() {
        out.push(Finding::ok(format!(
            "dependency graph: {} command(s), all `needs` resolve",
            known.len()
        )));
    }
    out
}

fn check_service_hints(config: &Config) -> Vec<Finding> {
    use scaffl_config::model::Backend as B;
    if !matches!(config.runtime.backend, B::Compose | B::Docker | B::Podman) {
        return Vec::new();
    }
    // We can't read docker-compose.yaml here without a YAML parser. So we
    // just count how many recipes declare `in =` and report it as info.
    let with_service = config
        .commands
        .values()
        .filter(|r| r.service.is_some())
        .count()
        + config
            .scripts
            .values()
            .filter(|s| s.service.is_some())
            .count();
    if with_service == 0 {
        Vec::new()
    } else {
        vec![Finding::ok(format!(
            "{with_service} command(s) target a container service (verify with `compose ps`)"
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scaffl_config::parse_str;

    #[test]
    fn unknown_dep_is_fail() {
        let cfg = parse_str(
            r#"
            [command.test]
            run = "true"
            needs = ["build"]
        "#,
        )
        .unwrap();
        let findings = check_dependency_graph(&cfg);
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Fail && f.message.contains("build"))
        );
    }

    #[test]
    fn known_dep_passes() {
        let cfg = parse_str(
            r#"
            [command.build]
            run = "true"
            [command.test]
            run = "true"
            needs = ["build"]
        "#,
        )
        .unwrap();
        let findings = check_dependency_graph(&cfg);
        assert!(findings.iter().all(|f| f.severity != Severity::Fail));
    }

    #[test]
    fn missing_env_file_is_warn() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = parse_str(
            r#"
            [env_files]
            files = [".env.does-not-exist"]
        "#,
        )
        .unwrap();
        let findings = check_env_files(&cfg, dir.path());
        assert!(findings.iter().any(|f| f.severity == Severity::Warn));
    }
}
