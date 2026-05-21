//! `croft worktree <action>` — inspect and pin worktree offsets.
//!
//! Three subcommands:
//!
//! - `status` — show identity + offset + computed isolation env for the
//!   current worktree. Read-only.
//! - `list` — enumerate every worktree returned by `git worktree list`,
//!   computing each one's offset against the current config. Useful for
//!   spotting hash collisions before they bite.
//! - `assign <name> <offset>` — write a `[worktrees.assign]` entry into
//!   `croft.toml` (or `.croft/local.toml` with `--local`). Uses
//!   `toml_edit` to preserve formatting and comments.

use crate::config::{Config, EnvSpec};
use crate::runtime::worktree::{BaseRef, Identity, WorktreeListEntry, list_worktrees, offset_for};
use anyhow::{Context, Result};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use std::collections::BTreeSet;
use std::path::Path;

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
        let project = config.project.name.as_deref().unwrap_or("croft");
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
    let entries = list_worktrees(project_root).await;
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
        eprintln!("         pin a slug with `croft worktree assign <name> <offset>` to dodge.");
    }
    Ok(())
}

pub fn assign(name: &str, offset: u32, local: bool, project_root: &Path) -> Result<()> {
    // Identity lookups always use the slugified form, so we slugify here
    // too — that way `croft worktree assign feature/x 5` produces a
    // `feature-x` entry that the runtime can match.
    let slug = crate::runtime::worktree::slugify(name);
    if slug.is_empty() {
        anyhow::bail!("`{name}` slugifies to an empty string; nothing to pin");
    }
    let target = if local {
        let dir = project_root.join(".croft");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        dir.join("local.toml")
    } else {
        project_root.join("croft.toml")
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

fn derive_slug(entry: &WorktreeListEntry) -> String {
    if let Some(branch) = entry.branch.as_deref() {
        return crate::runtime::worktree::slugify(branch);
    }
    if entry.detached {
        return crate::runtime::worktree::slugify(
            std::path::Path::new(&entry.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
        );
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Porcelain-parsing tests live in croft-runtime now (single
    // owner of the parser); only the assign / list-rendering paths
    // are tested from this crate.

    #[test]
    fn assign_writes_new_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("croft.toml"),
            r#"
[project]
name = "x"
"#,
        )
        .unwrap();

        assign("feature/x", 7, false, dir.path()).unwrap();
        let after = std::fs::read_to_string(dir.path().join("croft.toml")).unwrap();
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
        // No croft.toml; just write to local.
        assign("main", 0, true, dir.path()).unwrap();
        let local = std::fs::read_to_string(dir.path().join(".croft").join("local.toml")).unwrap();
        assert!(local.contains("main = 0"));
    }

    #[test]
    fn assign_updates_existing_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("croft.toml"),
            r#"
[worktrees.assign]
main = 0
feature-x = 5
"#,
        )
        .unwrap();
        assign("feature/x", 9, false, dir.path()).unwrap();
        let after = std::fs::read_to_string(dir.path().join("croft.toml")).unwrap();
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
