# Diff View

The diff view (`G` in the [TUI](TUI)) is a built-in
branch-review surface pinned to the merge-base with the project's
trunk branch.

Scope: every file that differs from the merge-base — committed-since-
branching plus working-tree changes plus untracked files (filtered
through `.gitignore`). **Not** the working-tree-vs-last-commit slice
that `git diff HEAD` shows.

## Trunk resolution

In order, first match wins:

1. `[diff].base = "..."` in `keel.toml` if set.
2. `git symbolic-ref refs/remotes/origin/HEAD` — the remote default
   branch when a remote is configured.
3. Local fallback: `main`, `master`, `develop`, `trunk`, in order.
4. None of the above → fall back to `git diff HEAD` so the view
   still works in repos with no trunk yet (fresh `git init`,
   detached repos, etc.).

The chosen trunk is surfaced in the top bar as
`<branch> vs <trunk>` so you always see what the file count and
per-file diffs are scoped against.

## Manual override

For projects that don't follow the conventional trunk names — pin
it explicitly:

```toml
[diff]
base = "release/stable"
```

Detection short-circuits before any git lookup.

## Refresh

The merge-base SHA is recomputed on every refresh (`r` in the diff
view, or any view-switch back to it), so `git pull origin main`
advancing the trunk shifts subsequent comparisons forward instead
of staying pinned to a stale base.

## Layout

- **Files panel** — every file that differs, with status flags (`M`
  modified, `A` added, `D` deleted, `?` untracked). `↑` / `↓`
  selects.
- **Body panel** — diff for the selected file. `Tab` cycles focus
  between files and body. In body focus, `↑` / `↓` scrolls; `]` /
  `[` jumps between hunks.

## Keymap

| Key | Context | Action |
|---|---|---|
| `Tab` | Always | Cycle focus files ↔ body. |
| `↑` / `↓` | Files | Move file selection. |
| `↑` / `↓` | Body | Scroll diff. |
| `]` / `[` | Body | Next / previous hunk. |
| `e` | Files | Open selected file in `$EDITOR` (see [TUI § Editor](TUI#editor-integration)). |
| `L` | Always | Hand off to `lazygit` (when installed). |
| `r` | Always | Refresh trunk + recompute merge-base. |
| `q` / `Esc` | Always | Back to dashboard. |

After an `e`-launched edit or an `L` lazygit session, keel marks the
diff stale and reloads automatically — changes show up without a
manual refresh.

## See also

- [TUI](TUI) for the dashboard the diff view is part of, including
  the editor + lazygit handoff details.
- [Configuration Reference: `[diff]`](Configuration-Reference#diff).
- [Configuration Reference: `[editor]`](Configuration-Reference#editor)
  for pinning a project editor.
