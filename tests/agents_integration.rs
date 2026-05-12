//! End-to-end integration: drive the apply pipeline against a
//! filesystem-path upstream that we mutate between runs to exercise
//! every code path (write / update / drift / orphan / once-mode).

use ampelos::agents::{AgentsError, AgentsState, ApplyOptions, FileMode, apply};
use ampelos::config::{
    AgentsConfig, MappingOverride, MappingOverrideKind, ResolvedOverride, SourceSpec,
};
use std::path::Path;
use tempfile::TempDir;
use tokio::process::Command;

async fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn init_upstream(dir: &Path) {
    run_git(dir, &["init", "-q", "-b", "main"]).await;
    run_git(dir, &["config", "user.email", "test@example.com"]).await;
    run_git(dir, &["config", "user.name", "Test"]).await;
}

async fn commit(dir: &Path, message: &str) {
    run_git(dir, &["add", "."]).await;
    run_git(dir, &["commit", "-q", "-m", message]).await;
}

fn cfg_with(source_repo: &Path, rev: &str) -> AgentsConfig {
    AgentsConfig {
        install_with_setup: true,
        manifest_path: "ampelos-agents.toml".into(),
        sources: vec![SourceSpec {
            name: "upstream".into(),
            repo: source_repo.to_string_lossy().into_owned(),
            rev: rev.into(),
            subpath: None,
            manifest_path: None,
            overrides: vec![],
        }],
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

#[tokio::test]
async fn install_then_update_then_orphan_removal() {
    let upstream = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    // Upstream layout: CLAUDE.md (replace), AGENTS.md (once),
    // skills/{a.md,b.md}.
    write(
        &upstream.path().join("ampelos-agents.toml"),
        r#"
        [[file]]
        src  = "agents/CLAUDE.md"
        dest = "CLAUDE.md"

        [[file]]
        src  = "agents/AGENTS.md"
        dest = "AGENTS.md"
        mode = "once"

        [[dir]]
        src  = "skills"
        dest = ".claude/skills"
        glob = "**/*.md"
        "#,
    );
    write(&upstream.path().join("agents/CLAUDE.md"), "v1 claude");
    write(&upstream.path().join("agents/AGENTS.md"), "v1 agents");
    write(&upstream.path().join("skills/a.md"), "skill a v1");
    write(&upstream.path().join("skills/b.md"), "skill b v1");
    init_upstream(upstream.path()).await;
    commit(upstream.path(), "v1").await;
    run_git(upstream.path(), &["tag", "v1.0.0"]).await;

    let cfg = cfg_with(upstream.path(), "v1.0.0");

    // First install — every file is new.
    let report = apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap();
    assert_eq!(
        report.written.len(),
        4,
        "all four files written: {report:?}"
    );
    assert_eq!(report.updated.len(), 0);
    assert_eq!(report.removed.len(), 0);
    assert!(report.unchanged.is_empty());
    assert!(report.drift_warnings.is_empty());
    assert!(report.collisions.is_empty());
    assert_eq!(
        std::fs::read_to_string(project.path().join("CLAUDE.md")).unwrap(),
        "v1 claude"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
        "v1 agents"
    );

    // Second install — everything matches state, nothing happens.
    let report = apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap();
    assert!(report.written.is_empty());
    assert!(report.updated.is_empty());
    assert!(report.removed.is_empty());
    // CLAUDE.md (replace) + 2 skills counts as unchanged. AGENTS.md
    // is once and reports as `once_kept`.
    assert_eq!(report.unchanged.len(), 3);
    assert_eq!(report.once_kept.len(), 1);

    // Update upstream: rewrite a skill, add a new one, remove the
    // other, change CLAUDE.md, mutate AGENTS.md (which we own once
    // and will NOT touch on re-apply).
    write(&upstream.path().join("agents/CLAUDE.md"), "v2 claude");
    write(&upstream.path().join("agents/AGENTS.md"), "v2 agents");
    std::fs::remove_file(upstream.path().join("skills/b.md")).unwrap();
    write(&upstream.path().join("skills/c.md"), "skill c v2");
    commit(upstream.path(), "v2").await;
    run_git(upstream.path(), &["tag", "v2.0.0"]).await;

    let cfg_v2 = cfg_with(upstream.path(), "v2.0.0");
    let report = apply(project.path(), &cfg_v2, &ApplyOptions::default())
        .await
        .unwrap();
    // CLAUDE.md updated, c.md new, b.md orphaned, a.md unchanged.
    assert!(report.updated.iter().any(|p| p.ends_with("CLAUDE.md")));
    assert!(
        report
            .written
            .iter()
            .any(|p| p.ends_with(".claude/skills/c.md"))
    );
    assert!(
        report
            .removed
            .iter()
            .any(|p| p.ends_with(".claude/skills/b.md"))
    );
    assert!(
        report
            .unchanged
            .iter()
            .any(|p| p.ends_with(".claude/skills/a.md"))
    );
    // AGENTS.md is once-mode; reapplied as "kept" not "updated".
    assert!(report.once_kept.iter().any(|p| p.ends_with("AGENTS.md")));
    assert_eq!(
        std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
        "v1 agents",
        "once-mode file must not be overwritten on update"
    );
    assert!(!project.path().join(".claude/skills/b.md").exists());
}

#[tokio::test]
async fn drift_is_detected_and_left_alone_by_default() {
    let upstream = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(
        &upstream.path().join("ampelos-agents.toml"),
        r#"
        [[file]]
        src  = "CLAUDE.md"
        dest = "CLAUDE.md"
        "#,
    );
    write(&upstream.path().join("CLAUDE.md"), "upstream");
    init_upstream(upstream.path()).await;
    commit(upstream.path(), "v1").await;
    run_git(upstream.path(), &["tag", "v1"]).await;

    let cfg = cfg_with(upstream.path(), "v1");
    apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap();

    // Hand-edit the ampelos-owned file.
    write(&project.path().join("CLAUDE.md"), "hand-edited");

    let report = apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap();
    assert_eq!(report.drift_warnings.len(), 1);
    assert!(report.updated.is_empty());
    assert_eq!(
        std::fs::read_to_string(project.path().join("CLAUDE.md")).unwrap(),
        "hand-edited",
        "drift must be left alone without --force-overwrite-drift"
    );

    // Force overwrite.
    let opts = ApplyOptions {
        force_overwrite_drift: true,
        ..Default::default()
    };
    let report = apply(project.path(), &cfg, &opts).await.unwrap();
    assert!(report.updated.iter().any(|p| p.ends_with("CLAUDE.md")));
    assert_eq!(
        std::fs::read_to_string(project.path().join("CLAUDE.md")).unwrap(),
        "upstream"
    );
}

#[tokio::test]
async fn local_sibling_in_dir_target_is_shadow_error() {
    let upstream = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(
        &upstream.path().join("ampelos-agents.toml"),
        r#"
        [[dir]]
        src  = "skills"
        dest = ".claude/skills"
        "#,
    );
    write(&upstream.path().join("skills/foo.md"), "upstream foo");
    init_upstream(upstream.path()).await;
    commit(upstream.path(), "v1").await;
    run_git(upstream.path(), &["tag", "v1"]).await;

    // User has a local foo.md before any install.
    write(&project.path().join(".claude/skills/foo.md"), "local foo");

    let cfg = cfg_with(upstream.path(), "v1");
    let err = apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, AgentsError::LocalShadow { .. }));
}

#[tokio::test]
async fn override_skip_drops_a_file() {
    let upstream = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(
        &upstream.path().join("ampelos-agents.toml"),
        r#"
        [[file]]
        src  = "CLAUDE.md"
        dest = "CLAUDE.md"

        [[file]]
        src  = "AGENTS.md"
        dest = "AGENTS.md"
        "#,
    );
    write(&upstream.path().join("CLAUDE.md"), "x");
    write(&upstream.path().join("AGENTS.md"), "y");
    init_upstream(upstream.path()).await;
    commit(upstream.path(), "v1").await;
    run_git(upstream.path(), &["tag", "v1"]).await;

    let mut cfg = cfg_with(upstream.path(), "v1");
    cfg.sources[0].overrides.push(MappingOverride {
        dest: "AGENTS.md".into(),
        action: Some(MappingOverrideKind::Skip),
        relocate: None,
    });
    // Sanity check the override resolves cleanly upfront.
    assert_eq!(
        cfg.sources[0].overrides[0].resolved().unwrap(),
        ResolvedOverride::Skip
    );

    let report = apply(project.path(), &cfg, &ApplyOptions::default())
        .await
        .unwrap();
    assert!(project.path().join("CLAUDE.md").exists());
    assert!(!project.path().join("AGENTS.md").exists());
    assert_eq!(report.written.len(), 1);
}

#[tokio::test]
async fn dry_run_writes_no_files_or_state() {
    let upstream = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(
        &upstream.path().join("ampelos-agents.toml"),
        r#"
        [[file]]
        src  = "CLAUDE.md"
        dest = "CLAUDE.md"
        "#,
    );
    write(&upstream.path().join("CLAUDE.md"), "x");
    init_upstream(upstream.path()).await;
    commit(upstream.path(), "v1").await;
    run_git(upstream.path(), &["tag", "v1"]).await;

    let cfg = cfg_with(upstream.path(), "v1");
    let opts = ApplyOptions {
        dry_run: true,
        ..Default::default()
    };
    let report = apply(project.path(), &cfg, &opts).await.unwrap();
    assert!(report.dry_run);
    assert_eq!(report.written.len(), 1);
    assert!(!project.path().join("CLAUDE.md").exists());
    assert!(AgentsState::load(project.path()).unwrap().is_none());
}

#[tokio::test]
async fn _unused_filemode_import_silenced() {
    // Compiler hint: keep `FileMode` exported for consumers without a
    // separate doc test.
    let _ = FileMode::Once;
}
