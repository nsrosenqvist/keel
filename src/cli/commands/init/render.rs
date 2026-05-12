//! Section-ordered TOML emitter for the auto-detected `ampelos.toml`.
//!
//! The detectors return shapeful fragments; this module is the *only*
//! place that knows what the output text looks like. Two responsibilities
//! the detectors deliberately don't carry:
//!
//! - **Commented-suggestion policy.** All `[command.*]` blocks emitted by
//!   the auto-detection path are commented out. The renderer prefixes
//!   every line so detectors needn't think about it.
//! - **Section ownership.** `[runtime]` and `[devcontainer]` are
//!   single-owner sections (first detector to claim them wins); env
//!   files are aggregated; commands with duplicate names are grouped
//!   under a "pick one" header.

use crate::cli::commands::init::detector::{CommandFragment, Finding, Fragment, RunSpec};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Build the full TOML body from the project name and the detector findings.
pub fn render(project_name: &str, findings: &[Finding]) -> String {
    let mut out = String::new();
    out.push_str("# ampelos configuration. See AGENTS.md / README.md for guidance.\n");
    out.push_str("# Auto-detected suggestions below are commented — uncomment what you want.\n\n");

    let _ = writeln!(out, "[project]\nname = \"{}\"\n", escape(project_name));

    render_runtime(&mut out, findings);
    render_devcontainer(&mut out, findings);
    render_env_files(&mut out, findings);
    render_commands(&mut out, findings);

    out
}

fn render_runtime(out: &mut String, findings: &[Finding]) {
    let runtimes: Vec<_> = findings
        .iter()
        .flat_map(|f| f.fragments.iter())
        .filter_map(|frag| match frag {
            Fragment::Runtime {
                backend,
                default_service,
                compose_passthrough,
                service_passthrough,
            } => Some((
                *backend,
                default_service.clone(),
                *compose_passthrough,
                *service_passthrough,
            )),
            _ => None,
        })
        .collect();

    out.push_str("[runtime]\n");
    if let Some((backend, default_service, compose_pt, service_pt)) = runtimes.first() {
        let _ = writeln!(out, "backend = \"{backend}\"");
        match default_service {
            Some(s) => {
                let _ = writeln!(out, "default_service = \"{}\"", escape(s));
            }
            None => out.push_str("# default_service = \"app\"\n"),
        }
        let _ = writeln!(out, "compose_passthrough = {compose_pt}");
        let _ = writeln!(out, "service_passthrough = {service_pt}");
    } else {
        out.push_str("backend = \"none\"\n");
    }
    out.push('\n');
}

fn render_devcontainer(out: &mut String, findings: &[Finding]) {
    let dc = findings
        .iter()
        .flat_map(|f| f.fragments.iter())
        .find_map(|frag| match frag {
            Fragment::Devcontainer { path } => Some(path.clone()),
            _ => None,
        });
    let Some(path) = dc else {
        return;
    };
    out.push_str("[devcontainer]\n");
    out.push_str("enabled = true\n");
    let _ = writeln!(out, "path = \"{}\"", escape(&path.to_string_lossy()));
    out.push('\n');
}

fn render_env_files(out: &mut String, findings: &[Finding]) {
    let mut seen = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    for frag in findings.iter().flat_map(|f| f.fragments.iter()) {
        if let Fragment::EnvFile(path) = frag
            && seen.insert(path.clone())
        {
            files.push(path.clone());
        }
    }
    if files.is_empty() {
        return;
    }
    out.push_str("[env_files]\nfiles = [");
    let inner: Vec<String> = files.iter().map(|p| format!("\"{}\"", escape(p))).collect();
    out.push_str(&inner.join(", "));
    out.push_str("]\n\n");
}

fn render_commands(out: &mut String, findings: &[Finding]) {
    // Group commands by name while preserving first-seen order.
    let mut groups: BTreeMap<String, Vec<(usize, &CommandFragment, &'static str)>> =
        BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut counter = 0usize;
    for finding in findings {
        for frag in &finding.fragments {
            if let Fragment::Command(c) = frag {
                let entry = groups.entry(c.name.clone()).or_default();
                if entry.is_empty() {
                    order.push(c.name.clone());
                }
                entry.push((counter, c, finding.ecosystem));
                counter += 1;
            }
        }
    }
    if order.is_empty() {
        return;
    }
    out.push_str("# Detected commands (uncomment to enable).\n\n");
    for name in &order {
        let group = &groups[name];
        if group.len() > 1 {
            let ecosystems: Vec<&str> = group.iter().map(|(_, _, eco)| *eco).collect();
            let _ = writeln!(
                out,
                "# Multiple ecosystems suggest `{name}` ({}). Uncomment ONE:",
                ecosystems.join(", ")
            );
        }
        for (i, (_, cmd, _)) in group.iter().enumerate() {
            render_command(out, name, cmd);
            if i + 1 < group.len() {
                out.push('\n');
            }
        }
        out.push('\n');
    }
}

fn render_command(out: &mut String, name: &str, c: &CommandFragment) {
    let keys = active_keys(c);
    let pad = keys.iter().map(|k| k.len()).max().unwrap_or(0);
    let _ = writeln!(out, "# [command.{name}]");
    emit_kv(out, "desc", &quoted(&c.desc), pad);
    emit_kv(out, "run", &format_run(&c.run), pad);
    if let Some(s) = &c.in_service {
        emit_kv(out, "in", &quoted(s), pad);
    }
    if let Some(tty) = c.tty {
        emit_kv(out, "tty", &format!("{tty}"), pad);
    }
    if let Some(fa) = c.forward_args {
        emit_kv(out, "forward_args", &format!("{fa}"), pad);
    }
    if !c.needs.is_empty() {
        let arr: Vec<String> = c.needs.iter().map(|s| quoted(s)).collect();
        emit_kv(out, "needs", &format!("[{}]", arr.join(", ")), pad);
    }
}

fn active_keys(c: &CommandFragment) -> Vec<&'static str> {
    let mut keys = vec!["desc", "run"];
    if c.in_service.is_some() {
        keys.push("in");
    }
    if c.tty.is_some() {
        keys.push("tty");
    }
    if c.forward_args.is_some() {
        keys.push("forward_args");
    }
    if !c.needs.is_empty() {
        keys.push("needs");
    }
    keys
}

fn emit_kv(out: &mut String, key: &str, value: &str, pad: usize) {
    let _ = writeln!(out, "# {key:<pad$} = {value}", pad = pad);
}

fn format_run(run: &RunSpec) -> String {
    match run {
        RunSpec::Single(s) => quoted(s),
        RunSpec::Steps(steps) => {
            let parts: Vec<String> = steps.iter().map(|s| quoted(s)).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::init::detector::CommandFragment;
    use std::path::PathBuf;

    fn finding(eco: &'static str, frags: Vec<Fragment>) -> Finding {
        Finding {
            ecosystem: eco,
            tool: None,
            fragments: frags,
            notes: Vec::new(),
        }
    }

    #[test]
    fn renders_default_runtime_none_when_no_runtime_fragment() {
        let body = render("p", &[]);
        assert!(body.contains("backend = \"none\""));
    }

    #[test]
    fn first_runtime_fragment_wins() {
        let f1 = finding(
            "compose",
            vec![Fragment::Runtime {
                backend: "compose",
                default_service: None,
                compose_passthrough: true,
                service_passthrough: true,
            }],
        );
        let f2 = finding(
            "other",
            vec![Fragment::Runtime {
                backend: "podman",
                default_service: None,
                compose_passthrough: true,
                service_passthrough: true,
            }],
        );
        let body = render("p", &[f1, f2]);
        assert!(body.contains("backend = \"compose\""));
        assert!(!body.contains("backend = \"podman\""));
    }

    #[test]
    fn env_files_dedupes() {
        let f = finding(
            "x",
            vec![
                Fragment::EnvFile(".env".into()),
                Fragment::EnvFile(".env".into()),
                Fragment::EnvFile(".env.local".into()),
            ],
        );
        let body = render("p", &[f]);
        let count = body.matches("\".env\"").count();
        assert_eq!(count, 1, "got: {body}");
        assert!(body.contains("\".env.local\""));
    }

    #[test]
    fn duplicate_command_name_emits_header_and_both() {
        let f = finding(
            "a",
            vec![
                Fragment::Command(CommandFragment::shell("test", "A", "a-test")),
                Fragment::Command(CommandFragment::shell("test", "B", "b-test")),
            ],
        );
        let body = render("p", &[f]);
        assert!(body.contains("Multiple ecosystems suggest `test`"));
        assert_eq!(body.matches("# [command.test]").count(), 2);
    }

    #[test]
    fn renders_devcontainer_section() {
        let f = finding(
            "devcontainer",
            vec![Fragment::Devcontainer {
                path: PathBuf::from(".devcontainer/devcontainer.json"),
            }],
        );
        let body = render("p", &[f]);
        assert!(body.contains("[devcontainer]"));
        assert!(body.contains("enabled = true"));
        assert!(body.contains(".devcontainer/devcontainer.json"));
    }

    #[test]
    fn output_parses_back_via_config() {
        let f1 = finding(
            "compose",
            vec![Fragment::Runtime {
                backend: "compose",
                default_service: Some("app".into()),
                compose_passthrough: true,
                service_passthrough: true,
            }],
        );
        let f2 = finding(
            "rust",
            vec![Fragment::Command(
                CommandFragment::shell("test", "Run tests", "cargo test --workspace")
                    .with_forward_args(),
            )],
        );
        let f3 = finding("dotenv", vec![Fragment::EnvFile(".env".into())]);
        let body = render("scratch", &[f1, f2, f3]);
        crate::config::parse_str(&body).expect("renders valid TOML");
    }
}
