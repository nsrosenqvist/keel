//! Git shell-outs + parsers for the Diff view.
//!
//! Everything ampelos knows about git at the process level for the diff
//! view lives here: the file-list query (`name-status` + `numstat` +
//! `ls-files --others` merged in parallel), per-file diff loaders,
//! read-mode loaders, and the porcelain-fallback for repos with no
//! resolvable trunk. The orchestration that wires these into
//! `DiffView` state runs through the actor-model entry points on
//! `App` (`request_diff_reload`, `request_diff_for_selected`,
//! `request_read_for_selected`) so handlers don't hold `&mut App`
//! across the await.
//!
//! Parsers are `pub(crate)` so the integration tests in `terminal.rs`
//! that exercise them can keep their existing assertions; everything
//! else is module-private or `pub(crate)` for spawned-task entry.

use crate::tui::views::diff::state::{
    DiffFile, DiffLine, DiffLineKind, DiffStatus, ReadLine, ReadLineKind,
};
use std::path::Path;

/// Resolve the current branch name (`git rev-parse --abbrev-ref HEAD`).
/// Returns None when detached or the command fails — the banner just
/// hides the branch slot in that case.
pub async fn current_branch(project_root: &Path) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}

/// Build the changed-file list. With `anchor` set, we want
/// "everything that differs from the merge-base, plus untracked
/// files" — that's `git diff --name-status <anchor>` (committed
/// since branching + working-tree changes against tracked files)
/// merged with `git ls-files --others --exclude-standard`
/// (currently-untracked files). Without an anchor, fall back to
/// `git status --porcelain` so we still work in repos where no
/// trunk could be detected (e.g. fresh `git init` with no commits
/// past HEAD).
pub async fn load_diff_files(
    project_root: &Path,
    anchor: Option<&str>,
) -> Result<Vec<DiffFile>, String> {
    use std::collections::BTreeMap;
    let Some(anchor) = anchor else {
        return load_diff_files_fallback(project_root).await;
    };

    // Three queries in parallel: name+status, churn (numstat), and
    // untracked. Saves ~100ms on cold cache vs the previous
    // sequential path.
    let diff_fut = tokio::process::Command::new("git")
        .args(["diff", "--name-status", anchor])
        .current_dir(project_root)
        .output();
    let numstat_fut = tokio::process::Command::new("git")
        .args(["diff", "--numstat", anchor])
        .current_dir(project_root)
        .output();
    let untracked_fut = tokio::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(project_root)
        .output();
    let (diff_out, numstat_out, untracked_out) = tokio::join!(diff_fut, numstat_fut, untracked_fut);
    let diff_out = diff_out.map_err(|e| format!("git diff --name-status failed: {e}"))?;
    if !diff_out.status.success() {
        // Anchor invalid (rare — `merge_base` already returned Some)
        // — fall back to porcelain so the view still works.
        return load_diff_files_fallback(project_root).await;
    }
    let numstat_out = numstat_out.map_err(|e| format!("git diff --numstat failed: {e}"))?;
    let untracked_out = untracked_out.map_err(|e| format!("git ls-files failed: {e}"))?;

    // Merge into a BTreeMap keyed by path so a file that's both
    // tracked-modified AND showing up in ls-files (shouldn't happen,
    // but defensive) doesn't appear twice.
    let mut files: BTreeMap<String, DiffFile> = BTreeMap::new();
    let diff_text = String::from_utf8_lossy(&diff_out.stdout);
    for entry in parse_diff_name_status(&diff_text) {
        files.insert(
            entry.path.clone(),
            DiffFile {
                path: entry.path,
                status: entry.status,
                additions: 0,
                deletions: 0,
                binary: false,
                old_path: entry.old_path,
            },
        );
    }
    if numstat_out.status.success() {
        let numstat_text = String::from_utf8_lossy(&numstat_out.stdout);
        for entry in parse_numstat(&numstat_text) {
            if let Some(f) = files.get_mut(&entry.path) {
                f.additions = entry.additions;
                f.deletions = entry.deletions;
                f.binary = entry.binary;
            }
        }
    }
    if untracked_out.status.success() {
        let untracked_text = String::from_utf8_lossy(&untracked_out.stdout);
        let paths: Vec<String> = untracked_text
            .lines()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        // Count `+` lines for each untracked file eagerly so the
        // sidebar shows the real number from the first frame
        // instead of `+0` until the body lazily loads. Reads run
        // in parallel; for typical untracked counts (<100) this
        // adds single-digit ms to the file-list load.
        let read_jobs = paths
            .iter()
            .map(|p| count_lines_in_file(project_root.join(p)));
        let counts = futures::future::join_all(read_jobs).await;
        for (path, additions) in paths.into_iter().zip(counts) {
            files.entry(path.clone()).or_insert_with(|| DiffFile {
                path,
                status: DiffStatus::Untracked,
                additions,
                deletions: 0,
                binary: false,
                old_path: None,
            });
        }
    }
    Ok(files.into_values().collect())
}

/// Count newline-terminated lines in a file. Used to populate the
/// `+N` churn for untracked files at file-list load time. Errors
/// collapse to 0 so a permission-denied or vanished file doesn't
/// break the whole list.
async fn count_lines_in_file(path: std::path::PathBuf) -> usize {
    match tokio::fs::read_to_string(&path).await {
        Ok(body) => body.lines().count(),
        Err(_) => 0,
    }
}

pub(crate) struct NumstatEntry {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

/// Parse `git diff --numstat <anchor>` output. Each line is
/// `<add>\t<del>\t<path>`. Binary files report `-\t-\t<path>`.
/// Rename destinations are resolved (see
/// `resolve_numstat_destination`) so the BTreeMap merge in the
/// caller — keyed by the path that `--name-status` produces —
/// picks up the churn for renamed files.
pub(crate) fn parse_numstat(input: &str) -> Vec<NumstatEntry> {
    let mut out = Vec::new();
    for line in input.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(add) = parts.next() else { continue };
        let Some(del) = parts.next() else { continue };
        let Some(path) = parts.next() else { continue };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let dest = resolve_numstat_destination(path);
        let binary = add == "-" && del == "-";
        let additions = if binary { 0 } else { add.parse().unwrap_or(0) };
        let deletions = if binary { 0 } else { del.parse().unwrap_or(0) };
        out.push(NumstatEntry {
            path: dest,
            additions,
            deletions,
            binary,
        });
    }
    out
}

/// Resolve the destination path inside a numstat row. `git diff
/// --numstat` represents renames two ways:
///
/// - Plain: `old => new` (no common prefix/suffix).
/// - Brace: `prefix{old => new}suffix`, where prefix and suffix
///   are the shared directory components, e.g.
///   `.{scaffl => ampelos}/commands/seed` for a top-level rename.
///
/// We rewrite the brace form by substituting the right side of the
/// `=>` and collapse any `//` left behind when either side is
/// empty (a renamed-away or renamed-into directory).
pub(crate) fn resolve_numstat_destination(path: &str) -> String {
    if let (Some(lb), Some(rb)) = (path.find('{'), path.rfind('}'))
        && lb < rb
    {
        let inside = &path[lb + 1..rb];
        if let Some(arrow) = inside.find(" => ") {
            let new_part = &inside[arrow + 4..];
            let mut out = String::with_capacity(path.len());
            out.push_str(&path[..lb]);
            out.push_str(new_part);
            out.push_str(&path[rb + 1..]);
            while out.contains("//") {
                out = out.replace("//", "/");
            }
            return out;
        }
    }
    if let Some(idx) = path.find(" => ") {
        return path[idx + 4..].to_string();
    }
    path.to_string()
}

/// Old behaviour, kept as a fallback when no trunk could be
/// detected: list whatever the working tree differs from HEAD on.
async fn load_diff_files_fallback(project_root: &Path) -> Result<Vec<DiffFile>, String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(project_root)
        .output()
        .await
        .map_err(|e| format!("git status failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git status exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(parse_status_porcelain(&stdout))
}

/// Parse `git diff --name-status <anchor>` output. Each line is one
/// status letter + tab + path (rename = `R<similarity>\told\tnew`).
/// Untracked files don't appear here — the caller pulls them
/// separately from `git ls-files --others`.
pub(crate) fn parse_diff_name_status(input: &str) -> Vec<DiffNameStatusEntry> {
    let mut out = Vec::new();
    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let Some(status_field) = parts.next() else {
            continue;
        };
        let letter = status_field.chars().next().unwrap_or(' ');
        let status = match letter {
            'A' => DiffStatus::Added,
            'D' => DiffStatus::Deleted,
            'M' => DiffStatus::Modified,
            'R' => DiffStatus::Renamed,
            'C' => DiffStatus::Other, // copy
            _ => DiffStatus::Other,
        };
        // Rename rows have two paths; we want the *destination* as
        // `path` and the *source* as `old_path` so the per-file diff
        // body can request a rename-aware diff. Empty path field →
        // malformed; skip rather than emit a row with an empty path
        // that would render as a blank sidebar entry.
        let (path, old_path) = match (parts.next(), parts.next()) {
            (Some(old), Some(new)) if !new.is_empty() => {
                let old = if old.is_empty() {
                    None
                } else {
                    Some(old.to_string())
                };
                (new.to_string(), old)
            }
            (Some(p), None) if !p.is_empty() => (p.to_string(), None),
            _ => continue,
        };
        out.push(DiffNameStatusEntry {
            path,
            status,
            old_path,
        });
    }
    out
}

pub(crate) struct DiffNameStatusEntry {
    pub path: String,
    pub status: DiffStatus,
    /// Source path for rename/copy rows; None otherwise.
    pub old_path: Option<String>,
}

/// Parse `git status --porcelain=v1` output. Each line is two
/// status chars + space + path (or `path -> renamed-to` for
/// renames). We pick the worst-of-the-two status chars to colour
/// the row; the file path is everything after.
pub(crate) fn parse_status_porcelain(input: &str) -> Vec<DiffFile> {
    let mut out = Vec::new();
    for line in input.lines() {
        if line.len() < 4 {
            continue;
        }
        let staged = line.as_bytes()[0] as char;
        let worktree = line.as_bytes()[1] as char;
        let rest = &line[3..];
        // Renames have the form `R  old -> new`.
        let path = if let Some(idx) = rest.find(" -> ") {
            rest[idx + 4..].to_string()
        } else {
            rest.to_string()
        };
        let status = match (staged, worktree) {
            ('?', '?') => DiffStatus::Untracked,
            ('A', _) | (_, 'A') => DiffStatus::Added,
            ('D', _) | (_, 'D') => DiffStatus::Deleted,
            ('R', _) | (_, 'R') => DiffStatus::Renamed,
            ('M', _) | (_, 'M') => DiffStatus::Modified,
            _ => DiffStatus::Other,
        };
        out.push(DiffFile {
            path,
            status,
            additions: 0,
            deletions: 0,
            binary: false,
            old_path: None,
        });
    }
    out
}

pub(crate) async fn load_diff_for_file(
    project_root: &Path,
    file: &DiffFile,
    anchor: Option<&str>,
) -> Vec<DiffLine> {
    // Untracked files don't exist in HEAD or the anchor — git diff
    // would error. Synthesise a file-as-added view with the file
    // contents prefixed by `+`.
    if file.status == DiffStatus::Untracked {
        return load_untracked_as_diff(project_root, &file.path).await;
    }
    let base = anchor.unwrap_or("HEAD");
    // Renames need both paths + `--find-renames`, otherwise git
    // sees the destination as a brand-new file from /dev/null and
    // reports every line as `+`, contradicting the sidebar's
    // rename-aware churn count.
    let mut args: Vec<&str> = vec!["diff", base];
    if let Some(old) = file.old_path.as_deref() {
        args.push("--find-renames");
        args.push("--");
        args.push(old);
        args.push(&file.path);
    } else {
        args.push("--");
        args.push(&file.path);
    }
    let output = match tokio::process::Command::new("git")
        .args(&args)
        .current_dir(project_root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return vec![DiffLine {
                kind: DiffLineKind::Header,
                text: format!("git diff failed: {e}"),
                old_lineno: None,
                new_lineno: None,
                spans: vec![],
            }];
        }
    };
    let body = String::from_utf8_lossy(&output.stdout);
    enrich_diff_lines(&body, &file.path)
}

/// Walk a unified-diff body and produce `DiffLine`s with per-line
/// line-numbers and syntect spans pre-computed.
///
/// Hunk headers (`@@ -A,B +C,D @@`) reset the `(old, new)` counters
/// so the gutter renders the same line numbers `git diff` would
/// print. Each non-hunk, non-header line goes through syntect once,
/// using the file's path to pick a syntax — avoids redoing the
/// lookup on every frame as the user scrolls.
pub(crate) fn enrich_diff_lines(body: &str, path: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;
    for raw in body.lines() {
        let kind = DiffLineKind::classify(raw);
        match kind {
            DiffLineKind::Hunk => {
                if let Some((o, n)) = parse_hunk_header(raw) {
                    old_no = o;
                    new_no = n;
                }
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                });
            }
            DiffLineKind::Header => {
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: None,
                    spans: vec![],
                });
            }
            DiffLineKind::Added => {
                let inner = raw.get(1..).unwrap_or("");
                let spans = crate::tui::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: None,
                    new_lineno: Some(new_no),
                    spans,
                });
                new_no = new_no.saturating_add(1);
            }
            DiffLineKind::Removed => {
                let inner = raw.get(1..).unwrap_or("");
                let spans = crate::tui::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: Some(old_no),
                    new_lineno: None,
                    spans,
                });
                old_no = old_no.saturating_add(1);
            }
            DiffLineKind::Context => {
                let inner = raw.strip_prefix(' ').unwrap_or(raw);
                let spans = crate::tui::syntax::highlight_inner(path, inner);
                out.push(DiffLine {
                    kind,
                    text: raw.to_string(),
                    old_lineno: Some(old_no),
                    new_lineno: Some(new_no),
                    spans,
                });
                old_no = old_no.saturating_add(1);
                new_no = new_no.saturating_add(1);
            }
        }
    }
    out
}

/// Parse the leading `(old_start, new_start)` out of a hunk header
/// like `@@ -10,7 +10,9 @@`. Returns None on malformed input —
/// callers leave the counters where they were, which is harmless.
pub(crate) fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // Skip the leading `@@`.
    let after = line.strip_prefix("@@")?;
    let mut tokens = after.split_whitespace();
    let old = tokens.next()?.strip_prefix('-')?;
    let new = tokens.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Walk a `DiffLine` stream and classify each new-side line as
/// Added, Modified, or Plain, plus emit Separator rows where pure
/// deletions occurred between two surviving lines. Group consecutive
/// non-context lines: if a group has both `+` and `-`, the `+` lines
/// are Modified (in-place edit); a `+`-only group is a pure Added
/// block; a `-`-only group becomes a Separator anchored to the
/// next surviving new-side line (or to the end of the file when
/// the deletion was at the tail).
pub(crate) fn annotate_read_with_diff(read: Vec<ReadLine>, diff: &[DiffLine]) -> Vec<ReadLine> {
    use std::collections::HashMap;
    let mut kind_by_lineno: HashMap<u32, ReadLineKind> = HashMap::new();
    // Key: new_lineno of the line immediately after the deletion.
    // `0` is reserved for "deletion at end of file" (no surviving
    // line after).
    let mut deletion_before: HashMap<u32, usize> = HashMap::new();

    let mut i = 0;
    while i < diff.len() {
        if !matches!(diff[i].kind, DiffLineKind::Added | DiffLineKind::Removed) {
            i += 1;
            continue;
        }
        let mut added_linenos: Vec<u32> = Vec::new();
        let mut removed = 0usize;
        while i < diff.len() {
            match diff[i].kind {
                DiffLineKind::Added => {
                    if let Some(n) = diff[i].new_lineno {
                        added_linenos.push(n);
                    }
                }
                DiffLineKind::Removed => removed += 1,
                _ => break,
            }
            i += 1;
        }
        if !added_linenos.is_empty() {
            let kind = if removed > 0 {
                ReadLineKind::Modified
            } else {
                ReadLineKind::Added
            };
            for n in added_linenos {
                kind_by_lineno.insert(n, kind);
            }
        } else if removed > 0 {
            let next = diff[i..].iter().find_map(|l| l.new_lineno).unwrap_or(0);
            *deletion_before.entry(next).or_insert(0) += removed;
        }
    }

    let mut out: Vec<ReadLine> = Vec::with_capacity(read.len() + deletion_before.len());
    for line in read {
        if let Some(&n) = deletion_before.get(&line.lineno) {
            out.push(ReadLine {
                kind: ReadLineKind::Separator { removed: n },
                lineno: 0,
                text: String::new(),
                spans: vec![],
            });
        }
        let kind = kind_by_lineno
            .get(&line.lineno)
            .copied()
            .unwrap_or(ReadLineKind::Plain);
        out.push(ReadLine { kind, ..line });
    }
    // Trailing deletion at the end of the file — there's no
    // surviving line after it, so emit the separator at the bottom.
    if let Some(&n) = deletion_before.get(&0) {
        out.push(ReadLine {
            kind: ReadLineKind::Separator { removed: n },
            lineno: 0,
            text: String::new(),
            spans: vec![],
        });
    }
    out
}

/// Load the full file contents for read mode. Working-tree copy for
/// present files; `git show <anchor>:<path>` for deleted files;
/// placeholder for binary blobs. I/O errors collapse to a single
/// error line so the renderer doesn't need a branch.
pub(crate) async fn load_read_for_file(
    project_root: &Path,
    file: &DiffFile,
    anchor: Option<&str>,
) -> Vec<ReadLine> {
    if file.binary {
        return vec![ReadLine {
            kind: ReadLineKind::Plain,
            lineno: 1,
            text: "binary file".into(),
            spans: vec![],
        }];
    }
    let body: Result<String, String> = if file.status == DiffStatus::Deleted {
        // Working-tree copy is gone; pull the pre-deletion contents
        // from the anchor (or HEAD if no anchor was resolved).
        let base = anchor.unwrap_or("HEAD");
        let spec = format!("{base}:{}", file.path);
        match tokio::process::Command::new("git")
            .args(["show", &spec])
            .current_dir(project_root)
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        let abs = project_root.join(&file.path);
        tokio::fs::read_to_string(&abs)
            .await
            .map_err(|e| e.to_string())
    };
    let body = match body {
        Ok(b) => b,
        Err(e) => {
            return vec![ReadLine {
                kind: ReadLineKind::Plain,
                lineno: 1,
                text: format!("could not read file: {e}"),
                spans: vec![],
            }];
        }
    };
    body.lines()
        .enumerate()
        .map(|(i, line)| ReadLine {
            kind: ReadLineKind::Plain,
            lineno: (i as u32).saturating_add(1),
            text: line.to_string(),
            spans: crate::tui::syntax::highlight_inner(&file.path, line),
        })
        .collect()
}

async fn load_untracked_as_diff(project_root: &Path, path: &str) -> Vec<DiffLine> {
    let abs = project_root.join(path);
    let body = tokio::fs::read_to_string(&abs).await.unwrap_or_default();
    let mut lines = vec![DiffLine {
        kind: DiffLineKind::Header,
        text: format!("untracked file: {path}"),
        old_lineno: None,
        new_lineno: None,
        spans: vec![],
    }];
    let mut new_no: u32 = 1;
    for l in body.lines() {
        let spans = crate::tui::syntax::highlight_inner(path, l);
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("+{l}"),
            old_lineno: None,
            new_lineno: Some(new_no),
            spans,
        });
        new_no = new_no.saturating_add(1);
    }
    lines
}
