//! `scaffl worktree <action>` — inspect and pin worktree offsets.
//!
//! Three subcommands:
//!
//! - `status` — show identity + offset + computed isolation env for the
//!   current worktree. Read-only.
//! - `list` — enumerate every worktree returned by `git worktree list`,
//!   computing each one's offset against the current config. Useful for
//!   spotting hash collisions before they bite.
//! - `assign <name> <offset>` — write a `[worktrees.assign]` entry into
//!   `scaffl.toml` (or `.scaffl/local.toml` with `--local`). Uses
//!   `toml_edit` to preserve formatting and comments.

use anyhow::{Context, Result};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use scaffl_config::{Config, EnvSpec};
use scaffl_runtime::worktree::{BaseRef, Identity, offset_for};
use std::collections::BTreeSet;
use std::path::Path;
use tokio::process::Command;

pub async fn status(config: &Config, identity: &Identity) -> Result<()> {
    println!("slug:       {}", display_slug(&identity.slug));
    println!("base ref:   {}", identity.base_ref.label());
    println!(
        "base kind:  {}",
        match identity.base_ref {
            BaseRef::Branch(_) => "branch",
            BaseRef::DetachedSha(_) => "detached SHA",
            BaseRef::WorktreeDir(_) => "worktree dir",
            BaseRef::None => "none (not a git repo)",
        }
    );
    println!("offset:     {}", identity.offset);
    println!(
        "modulus:    {} (assignment: {})",
        config.worktrees.modulus,
        if config.worktrees.assign.contains_key(&identity.slug) {
            "pinned"
        } else if identity.slug.is_empty() {
            "n/a"
        } else {
            "hash"
        },
    );
    println!(
        "isolate:    {} (compose project name {})",
        config.worktrees.isolate_compose,
        if !identity.is_isolated() {
            "skipped — no slug"
        } else if !config.worktrees.isolate_compose {
            "skipped — disabled"
        } else {
            "applied"
        },
    );
    if config.worktrees.isolate_compose && identity.is_isolated() {
        let project = config.project.name.as_deref().unwrap_or("scaffl");
        println!("                  → {project}-{}", identity.slug);
    }

    // Surface any [env] entries that use the base + offset arithmetic
    // — the most common reason users care about worktree offsets.
    let derived: Vec<(&String, &EnvSpec)> = config
        .env
        .iter()
        .filter(|(_, spec)| spec.base.is_some())
        .collect();
    if !derived.is_empty() {
        println!("\nderived env:");
        for (name, spec) in derived {
            let base: i64 = spec.base.as_deref().unwrap_or("0").parse().unwrap_or(0);
            let resolved = base + i64::from(identity.offset);
            println!(
                "  {name:<20} = {resolved}  (base {base} + offset {})",
                identity.offset
            );
        }
    }
    Ok(())
}

pub async fn list(config: &Config, project_root: &Path) -> Result<()> {
    let entries = git_worktrees(project_root).await;
    if entries.is_empty() {
        println!("No git worktrees detected (run from inside a git repo).");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["worktree", "branch", "slug", "offset", "source"]);

    let mut offsets_seen: BTreeSet<u32> = BTreeSet::new();
    let mut collisions: BTreeSet<u32> = BTreeSet::new();

    for entry in &entries {
        let slug = derive_slug(entry);
        let pinned = config.worktrees.assign.contains_key(&slug);
        let offset = offset_for(
            &slug,
            &config.worktrees.assign,
            config.worktrees.modulus,
            config.resolved_seed(),
        );
        if !slug.is_empty() && !offsets_seen.insert(offset) {
            collisions.insert(offset);
        }
        table.add_row(vec![
            entry.path.clone(),
            entry.branch.clone().unwrap_or_else(|| "<detached>".into()),
            if slug.is_empty() {
                "<empty>".into()
            } else {
                slug
            },
            offset.to_string(),
            (if pinned { "pinned" } else { "hash" }).into(),
        ]);
    }
    println!("{table}");

    if !collisions.is_empty() {
        eprintln!(
            "\nwarning: {} offset(s) collide between worktrees: {:?}",
            collisions.len(),
            collisions,
        );
        eprintln!("         pin a slug with `scaffl worktree assign <name> <offset>` to dodge.");
    }
    Ok(())
}

pub fn assign(name: &str, offset: u32, local: bool, project_root: &Path) -> Result<()> {
    // Identity lookups always use the slugified form, so we slugify here
    // too — that way `scaffl worktree assign feature/x 5` produces a
    // `feature-x` entry that the runtime can match.
    let slug = scaffl_runtime::worktree::slugify(name);
    if slug.is_empty() {
        anyhow::bail!("`{name}` slugifies to an empty string; nothing to pin");
    }
    let target = if local {
        let dir = project_root.join(".scaffl");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        dir.join("local.toml")
    } else {
        project_root.join("scaffl.toml")
    };

    let raw = if target.exists() {
        std::fs::read_to_string(&target).with_context(|| format!("read {}", target.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parse {}", target.display()))?;

    // Ensure [worktrees] and [worktrees.assign] tables exist.
    let worktrees = doc
        .entry("worktrees")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .context("`worktrees` is not a table")?;
    let assign_tbl = worktrees
        .entry("assign")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .context("`worktrees.assign` is not a table")?;

    assign_tbl[slug.as_str()] = toml_edit::value(i64::from(offset));

    std::fs::write(&target, doc.to_string())
        .with_context(|| format!("write {}", target.display()))?;
    if slug == name {
        println!("Pinned `{slug}` → offset {offset} in {}", target.display());
    } else {
        println!(
            "Pinned `{slug}` (from `{name}`) → offset {offset} in {}",
            target.display()
        );
    }
    Ok(())
}

fn display_slug(slug: &str) -> &str {
    if slug.is_empty() { "<empty>" } else { slug }
}

#[derive(Debug, Clone)]
struct WorktreeEntry {
    path: String,
    branch: Option<String>,
    detached: bool,
}

fn derive_slug(entry: &WorktreeEntry) -> String {
    if let Some(branch) = entry.branch.as_deref() {
        return scaffl_runtime::worktree::slugify(branch);
    }
    if entry.detached {
        // Use last 7 chars of the worktree dir path as a fallback handle.
        return scaffl_runtime::worktree::slugify(
            std::path::Path::new(&entry.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
        );
    }
    String::new()
}

/// Parse `git worktree list --porcelain` output. Returns one entry per
/// worktree.
async fn git_worktrees(project_root: &Path) -> Vec<WorktreeEntry> {
    let Ok(output) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse_worktree_porcelain(&stdout)
}

fn parse_worktree_porcelain(input: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut detached = false;
    for line in input.lines() {
        if line.is_empty() {
            if let Some(p) = path.take() {
                out.push(WorktreeEntry {
                    path: p,
                    branch: branch.take(),
                    detached: std::mem::take(&mut detached),
                });
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            // `branch refs/heads/feature/x` → strip the prefix.
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        } else if line == "detached" {
            detached = true;
        }
    }
    if let Some(p) = path {
        out.push(WorktreeEntry {
            path: p,
            branch,
            detached,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_porcelain_minimal() {
        let input = "\
worktree /home/me/proj
HEAD abcd1234
branch refs/heads/main

worktree /home/me/proj-feature
HEAD ef567890
branch refs/heads/feature/x
";
        let entries = parse_worktree_porcelain(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "/home/me/proj");
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(!entries[0].detached);
        assert_eq!(entries[1].branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn parses_detached_entry() {
        let input = "\
worktree /home/me/proj-detached
HEAD abcd1234
detached
";
        let entries = parse_worktree_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].detached);
        assert!(entries[0].branch.is_none());
    }

    #[test]
    fn assign_writes_new_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("scaffl.toml"),
            r#"
[project]
name = "x"
"#,
        )
        .unwrap();

        assign("feature/x", 7, false, dir.path()).unwrap();
        let after = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(after.contains("[worktrees]"));
        assert!(after.contains("[worktrees.assign]"));
        // Slugified key — runtime lookup uses the slug form too.
        assert!(after.contains("feature-x = 7"));
        // Original [project] block survives.
        assert!(after.contains("name = \"x\""));
    }

    #[test]
    fn assign_local_writes_to_local_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        // No scaffl.toml; just write to local.
        assign("main", 0, true, dir.path()).unwrap();
        let local = std::fs::read_to_string(dir.path().join(".scaffl").join("local.toml")).unwrap();
        assert!(local.contains("main = 0"));
    }

    #[test]
    fn assign_updates_existing_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("scaffl.toml"),
            r#"
[worktrees.assign]
main = 0
feature-x = 5
"#,
        )
        .unwrap();
        assign("feature/x", 9, false, dir.path()).unwrap();
        let after = std::fs::read_to_string(dir.path().join("scaffl.toml")).unwrap();
        assert!(after.contains("feature-x = 9"));
        assert!(after.contains("main = 0"));
    }

    #[test]
    fn assign_rejects_empty_slug() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = assign("///", 1, false, dir.path()).unwrap_err();
        assert!(err.to_string().contains("empty string"));
    }
}
