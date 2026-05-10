//! Marker-delimited "scaffl-managed" block writer.
//!
//! Several scaffl features write structured content into files the user
//! also edits — the worktree-derived dotenv block, the auto-generated
//! `.scaffl/.gitignore`, and (eventually) anything else that needs to
//! coexist with hand-edited lines. This module centralises the markers,
//! the strip/replace algorithm, and the idempotent write logic so each
//! feature stops reinventing them.
//!
//! Contract:
//!
//! - On write, a single marker-delimited block holds the `body` provided.
//!   Content above and below the block is preserved verbatim.
//! - Re-writing the same body is a no-op: the file isn't touched and its
//!   mtime is left alone, so file watchers and `git status` stay quiet.
//! - Parent directories are created as needed.

use std::io;
use std::path::Path;

/// Opening marker line. Match-prefix tolerant so old files with slightly
/// different suffixes (`(auto)`, `(auto-generated)`, …) still strip
/// cleanly during the migration window.
pub const BEGIN_MARKER: &str = "# >>> scaffl-managed (auto-generated; do not edit by hand) >>>";

/// Closing marker line.
pub const END_MARKER: &str = "# <<< scaffl-managed <<<";

const BEGIN_PREFIX: &str = "# >>> scaffl-managed";
const END_PREFIX: &str = "# <<< scaffl-managed";

/// Idempotent write: replace (or insert) the scaffl-managed block in
/// `path` with `body`. Returns `true` when the file was modified,
/// `false` when the on-disk contents already matched.
///
/// `body` may be empty (the block is still written with just the
/// markers) or multi-line; a trailing newline is supplied automatically
/// so callers don't have to worry about it.
pub fn write(path: &Path, body: &str) -> io::Result<bool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let preserved = strip(&existing);

    let mut content = preserved;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(BEGIN_MARKER);
    content.push('\n');
    if !body.is_empty() {
        content.push_str(body);
        if !body.ends_with('\n') {
            content.push('\n');
        }
    }
    content.push_str(END_MARKER);
    content.push('\n');

    if content == existing {
        return Ok(false);
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// Drop the scaffl-managed block (markers and every line between) from
/// `content`, preserving everything else. Tolerates absent block and
/// minor variations in the marker suffix.
pub fn strip(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inside = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(BEGIN_PREFIX) {
            inside = true;
            continue;
        }
        if trimmed.starts_with(END_PREFIX) {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    // The strip path may have left trailing blank lines where the block
    // used to sit; the writer appends its own trailing newline, so trim
    // the doubled separators here.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn strip_returns_input_when_no_block() {
        assert_eq!(strip("A=1\nB=2\n"), "A=1\nB=2\n");
    }

    #[test]
    fn strip_removes_block_and_keeps_surroundings() {
        let content = "A=1\n# >>> scaffl-managed (auto) >>>\nFOO=bar\nBAZ=qux\n# <<< scaffl-managed <<<\nB=2\n";
        assert_eq!(strip(content), "A=1\nB=2\n");
    }

    #[test]
    fn write_creates_block_in_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("file.txt");
        let modified = write(&path, "first\nsecond\n").unwrap();
        assert!(modified);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(BEGIN_MARKER));
        assert!(body.contains("first"));
        assert!(body.contains("second"));
        assert!(body.contains(END_MARKER));
    }

    #[test]
    fn write_preserves_user_content_above_and_below() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "USER=top\nSECRET=keep\n").unwrap();
        write(&path, "MANAGED=value\n").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("USER=top"));
        assert!(body.contains("SECRET=keep"));
        assert!(body.contains("MANAGED=value"));
    }

    #[test]
    fn write_is_idempotent_on_repeat() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        assert!(write(&path, "X=1\n").unwrap());
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!write(&path, "X=1\n").unwrap());
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after);
    }

    #[test]
    fn write_replaces_existing_block_in_place() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(
            &path,
            "A=1\n# >>> scaffl-managed (auto) >>>\nOLD=1\n# <<< scaffl-managed <<<\nB=2\n",
        )
        .unwrap();
        write(&path, "NEW=1\n").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("NEW=1"));
        assert!(!body.contains("OLD=1"));
        assert!(body.contains("A=1"));
        assert!(body.contains("B=2"));
        assert_eq!(body.matches(BEGIN_PREFIX).count(), 1);
    }

    #[test]
    fn write_accepts_empty_body() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        write(&path, "").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(BEGIN_MARKER));
        assert!(body.contains(END_MARKER));
    }
}
