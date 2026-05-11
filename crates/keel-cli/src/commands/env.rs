//! `keel env` — print the resolved project environment, or
//! materialise it to a dotenv file with a keel-managed block.
//!
//! The `--write` form is what makes the worktree-aware `[env]`
//! arithmetic visible to tools that don't go through keel's process
//! tree (e.g. plain `docker compose up`, IDE-launched servers, etc.).
//! Hooked up to `post-checkout` / `post-merge`, the file is rewritten
//! on every branch switch, so dotenv-aware tooling automatically sees
//! the right values for the active worktree.

use anyhow::{Context, Result};
use keel_config::Config;
use keel_config::managed_block;
use keel_runtime::Env;
use std::path::{Path, PathBuf};

/// Resolve the project env (process + .env files + `[env]` section)
/// and either print sorted `KEY=VALUE` pairs to stdout (default) or
/// write them as a `# >>> keel-managed >>>` block inside a file.
pub async fn run(config: &Config, project_root: &Path, write: Option<PathBuf>) -> Result<()> {
    let env = Env::resolve(config, project_root).await?;

    let exported = exportable_keys(config);
    let pairs: Vec<(&str, &str)> = env.iter().filter(|(k, _)| exported.contains(*k)).collect();

    match write {
        None => {
            for (k, v) in pairs {
                println!("{k}={v}");
            }
        }
        Some(path) => {
            let resolved_path = resolve_path(project_root, &path);
            let changed = write_managed_block(&resolved_path, &pairs)?;
            if changed {
                println!(
                    "Wrote {} keel-managed entries to {}",
                    pairs.len(),
                    resolved_path.display()
                );
            } else {
                println!("{} already up to date", resolved_path.display());
            }
        }
    }
    Ok(())
}

/// Auto-write the resolved env to the path configured in
/// `[worktrees] dotenv`. Used by both the app's pre-dispatch
/// auto-write and the built-in env-rewrite hook handler. Silent on
/// success; returns `Ok(())` and does nothing if `[worktrees] dotenv`
/// isn't set.
pub async fn auto_write_if_configured(config: &Config, project_root: &Path) -> Result<()> {
    let Some(rel) = config.worktrees.dotenv.as_deref() else {
        return Ok(());
    };
    let env = Env::resolve(config, project_root).await?;
    let exported = exportable_keys(config);
    let pairs: Vec<(&str, &str)> = env.iter().filter(|(k, _)| exported.contains(*k)).collect();
    let path = resolve_path(project_root, Path::new(rel));
    write_managed_block(&path, &pairs)?;
    Ok(())
}

fn resolve_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

/// The set of env vars keel is willing to export to a file. We
/// curate this rather than dumping every inherited shell variable —
/// the file becomes huge and noisy otherwise. Includes:
///
/// - Every key explicitly declared in `[env]`.
/// - The three worktree-derived built-ins keel injects.
fn exportable_keys(config: &Config) -> std::collections::BTreeSet<String> {
    let mut keys: std::collections::BTreeSet<String> = config.env.keys().cloned().collect();
    keys.insert("KEEL_WORKTREE_SLUG".into());
    keys.insert("KEEL_WORKTREE_OFFSET".into());
    keys.insert("COMPOSE_PROJECT_NAME".into());
    keys
}

/// Idempotent write: builds the dotenv body from `pairs` and asks the
/// shared `managed_block` helper to splice it into `path`. Returns
/// `true` when the file changed, `false` when it was already up to
/// date. Mtime is left alone on no-op runs.
fn write_managed_block(path: &Path, pairs: &[(&str, &str)]) -> Result<bool> {
    let mut body = String::new();
    for (k, v) in pairs {
        body.push_str(&format!("{}={}\n", k, format_value(v)));
    }
    managed_block::write(path, &body).with_context(|| format!("write {}", path.display()))
}

/// Quote dotenv values containing whitespace, quotes, or backslashes
/// so common parsers (compose, dotenvy, sh `set -a; . .env`) all
/// agree on the boundaries. Values without those characters round-
/// trip unchanged.
fn format_value(value: &str) -> String {
    let needs_quotes = value
        .chars()
        .any(|c| c.is_ascii_whitespace() || c == '"' || c == '\\' || c == '#');
    if !needs_quotes {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_config::managed_block::{BEGIN_MARKER, END_MARKER};
    use tempfile::TempDir;

    #[test]
    fn format_value_passes_through_simple() {
        assert_eq!(format_value("8080"), "8080");
        assert_eq!(format_value("feature-x"), "feature-x");
    }

    #[test]
    fn format_value_quotes_whitespace_and_quotes() {
        assert_eq!(format_value("a b"), "\"a b\"");
        assert_eq!(format_value("with \"quote\""), "\"with \\\"quote\\\"\"");
        assert_eq!(
            format_value("path\\with\\slash"),
            "\"path\\\\with\\\\slash\""
        );
        assert_eq!(format_value("two\nlines"), "\"two\\nlines\"");
    }

    #[test]
    fn write_emits_dotenv_pairs_inside_managed_block() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("subdir").join(".env");
        let pairs = vec![("APP_PORT", "8085"), ("KEEL_WORKTREE_SLUG", "feature-x")];
        write_managed_block(&path, &pairs).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(BEGIN_MARKER));
        assert!(body.contains("APP_PORT=8085"));
        assert!(body.contains("KEEL_WORKTREE_SLUG=feature-x"));
        assert!(body.contains(END_MARKER));
    }

    #[test]
    fn write_is_idempotent_on_unchanged_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".env");
        let pairs = vec![("APP_PORT", "8085")];
        let first = write_managed_block(&path, &pairs).unwrap();
        assert!(first, "first write should report modified");
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let second = write_managed_block(&path, &pairs).unwrap();
        assert!(!second, "second write should report unchanged");
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "mtime must not advance");
    }
}
