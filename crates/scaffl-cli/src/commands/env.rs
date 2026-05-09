//! `scaffl env` — print the resolved project environment, or
//! materialise it to a dotenv file with a scaffl-managed block.
//!
//! The `--write` form is what makes the worktree-aware `[env]`
//! arithmetic visible to tools that don't go through scaffl's process
//! tree (e.g. plain `docker compose up`, IDE-launched servers, etc.).
//! Hooked up to `post-checkout` / `post-merge`, the file is rewritten
//! on every branch switch, so dotenv-aware tooling automatically sees
//! the right values for the active worktree.

use anyhow::{Context, Result};
use scaffl_config::Config;
use scaffl_runtime::Env;
use std::path::{Path, PathBuf};

/// Markers used to delimit the scaffl-managed block inside a target
/// file. Matched verbatim — keep them stable so existing files round-
/// trip cleanly.
const BEGIN_MARKER: &str = "# >>> scaffl-managed (auto-generated; do not edit by hand) >>>";
const END_MARKER: &str = "# <<< scaffl-managed <<<";

/// Resolve the project env (process + .env files + `[env]` section)
/// and either print sorted `KEY=VALUE` pairs to stdout (default) or
/// write them as a `# >>> scaffl-managed >>>` block inside a file.
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
                    "Wrote {} scaffl-managed entries to {}",
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

/// The set of env vars scaffl is willing to export to a file. We
/// curate this rather than dumping every inherited shell variable —
/// the file becomes huge and noisy otherwise. Includes:
///
/// - Every key explicitly declared in `[env]`.
/// - The three worktree-derived built-ins scaffl injects.
fn exportable_keys(config: &Config) -> std::collections::BTreeSet<String> {
    let mut keys: std::collections::BTreeSet<String> = config.env.keys().cloned().collect();
    keys.insert("SCAFFL_WORKTREE_SLUG".into());
    keys.insert("SCAFFL_WORKTREE_OFFSET".into());
    keys.insert("COMPOSE_PROJECT_NAME".into());
    keys
}

/// Idempotent write: produces the desired content, compares against
/// what's on disk, and only writes when they differ. Returns `true`
/// if the file was modified, `false` if it was already up to date.
/// Avoids touching mtime on no-op runs — callers fire this on every
/// scaffl invocation and surrounding tooling (file watchers, build
/// systems) shouldn't see spurious changes.
fn write_managed_block(path: &Path, pairs: &[(&str, &str)]) -> Result<bool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let preserved = strip_managed_block(&existing);

    let mut content = preserved;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(BEGIN_MARKER);
    content.push('\n');
    for (k, v) in pairs {
        content.push_str(&format!("{}={}\n", k, format_value(v)));
    }
    content.push_str(END_MARKER);
    content.push('\n');

    if content == existing {
        return Ok(false);
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Drop the scaffl-managed block (markers and all lines between)
/// while preserving everything else. Tolerates absent block.
fn strip_managed_block(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inside = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("# >>> scaffl-managed") {
            inside = true;
            continue;
        }
        if trimmed.starts_with("# <<< scaffl-managed") {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    // Drop trailing empty lines created by the strip — the writer
    // adds its own trailing newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
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
    fn strip_round_trips() {
        let content =
            "A=1\n# >>> scaffl-managed (auto) >>>\nFOO=bar\n# <<< scaffl-managed <<<\nB=2\n";
        let stripped = strip_managed_block(content);
        assert_eq!(stripped, "A=1\nB=2\n");
    }

    #[test]
    fn strip_handles_absent_block() {
        let content = "A=1\nB=2\n";
        assert_eq!(strip_managed_block(content), "A=1\nB=2\n");
    }

    #[test]
    fn write_creates_block_in_new_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("subdir").join(".env");
        let pairs = vec![("APP_PORT", "8085"), ("SCAFFL_WORKTREE_SLUG", "feature-x")];
        write_managed_block(&path, &pairs).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(BEGIN_MARKER));
        assert!(body.contains("APP_PORT=8085"));
        assert!(body.contains("SCAFFL_WORKTREE_SLUG=feature-x"));
        assert!(body.contains(END_MARKER));
    }

    #[test]
    fn write_preserves_user_content_around_block() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "USER_VAR=keep\nSECRET=keep-this\n").unwrap();
        let pairs = vec![("APP_PORT", "8085")];
        write_managed_block(&path, &pairs).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("USER_VAR=keep"));
        assert!(body.contains("SECRET=keep-this"));
        assert!(body.contains("APP_PORT=8085"));
    }

    #[test]
    fn write_is_idempotent_on_unchanged_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".env");
        let pairs = vec![("APP_PORT", "8085")];
        let first = write_managed_block(&path, &pairs).unwrap();
        assert!(first, "first write should report modified");
        // Capture mtime before second write.
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Re-running with the same pairs must NOT touch the file.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let second = write_managed_block(&path, &pairs).unwrap();
        assert!(!second, "second write should report unchanged");
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "mtime must not advance");
    }

    #[test]
    fn write_replaces_old_block_in_place() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            "USER_VAR=keep\n# >>> scaffl-managed (auto) >>>\nAPP_PORT=8080\n# <<< scaffl-managed <<<\nMORE=ok\n",
        )
        .unwrap();
        let pairs = vec![("APP_PORT", "8085")];
        write_managed_block(&path, &pairs).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("APP_PORT=8085"));
        assert!(!body.contains("APP_PORT=8080"));
        assert!(body.contains("USER_VAR=keep"));
        assert!(body.contains("MORE=ok"));
        // Exactly one managed block.
        assert_eq!(body.matches(BEGIN_MARKER).count(), 1);
    }
}
