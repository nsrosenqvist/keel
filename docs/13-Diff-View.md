# Diff View

The diff view is a branch-review surface inside the [TUI](12-TUI):
every file you've changed since you branched off `main` (or
whatever your trunk is), with hunk navigation, jump-to-editor,
and lazygit handoff. Press **`G`** to open it.

It shows **everything since the merge-base**, not the
working-tree-vs-last-commit slice that `git diff HEAD` shows.
Committed changes, working-tree changes, and untracked files
(filtered through `.gitignore`) all appear.

## Quickstart

```sh
croft        # open the dashboard
G           # open the diff view
```

```
↑ / ↓   move between files
Tab     switch focus to the diff body
↑ / ↓   scroll the diff (when body focused)
] / [   next / previous hunk
e       open the selected file in your editor
L       hand off to lazygit
r       refresh (recompute merge-base)
q / Esc back to dashboard
```

The top bar shows `<your-branch> vs <trunk>` so you always see
what the file list is scoped against.

## Mental model

- **Anchored to a merge-base, not to `HEAD`.** The view answers
  "what would my PR contain?" — everything since you forked off
  the trunk, not just uncommitted edits.
- **Trunk is auto-detected.** Conventional names (`main`,
  `master`, `develop`, `trunk`) work without config. The
  detection order is documented below; pin manually if your
  project uses something else.
- **Refreshing keeps it honest.** Press `r` (or switch views and
  back) to recompute the merge-base after pulling — otherwise the
  comparison can stay pinned to a stale trunk SHA.

## Common tasks

### Review what your branch will introduce

`G` from the dashboard. Use `↑` / `↓` to walk the file list;
`Tab` into the body and `]` / `[` jump between hunks of the
selected file.

### Use a non-standard trunk

```toml
[diff]
base = "release/stable"
```

Detection short-circuits before any git lookup — useful for
projects where `main` isn't the integration branch.

### Edit a file you're reviewing

In the files panel, press `e` on the selected row. croft opens
your `[editor].command` / `$VISUAL` / `$EDITOR` / `vim` (in that
order). Terminal editors suspend the TUI and resume on exit;
GUI editors spawn detached. After a terminal-editor session,
the diff reloads automatically. See
[TUI § Editor integration](TUI#editor-integration).

### Hand off to lazygit

`L` from the diff view. croft leaves the alternate screen, runs
lazygit foreground, and re-enters when you `q` out. Commits /
stages / resets done inside lazygit invalidate the cached diff —
croft reloads automatically. No-op (with a hint flashed in the
status bar) when lazygit isn't on `PATH`.

### Refresh after a rebase / pull

`r` recomputes the merge-base. Use after `git pull origin main`
shifts the trunk forward — otherwise `<your-branch> vs <trunk>`
keeps comparing against the pre-pull SHA.

## Reference

### Scope

Every file that differs from the merge-base with the resolved
trunk:

- Committed-since-branching changes.
- Working-tree changes (staged + unstaged).
- Untracked files (filtered through `.gitignore`).

Status flags in the files panel: `M` modified, `A` added, `D`
deleted, `?` untracked.

### Trunk resolution

In order, first match wins:

1. `[diff].base = "..."` in `croft.toml` if set.
2. `git symbolic-ref refs/remotes/origin/HEAD` — the remote
   default branch when a remote is configured.
3. Local fallback: `main`, `master`, `develop`, `trunk`, in
   that order.
4. None of the above → fall back to `git diff HEAD` so the view
   still works in repos with no trunk (fresh `git init`, detached
   repos, etc.).

### Layout

- **Files panel** — every file that differs, with status flags.
  `↑` / `↓` selects.
- **Body panel** — diff for the selected file. `Tab` cycles
  focus between files and body. In body focus, `↑` / `↓`
  scrolls; `]` / `[` jumps between hunks.

### Keymap

| Key | Context | Action |
|---|---|---|
| `Tab` | Always | Cycle focus files ↔ body. |
| `↑` / `↓` | Files | Move file selection. |
| `↑` / `↓` | Body | Scroll diff. |
| `]` / `[` | Body | Next / previous hunk. |
| `e` | Files | Open selected file in `$EDITOR` — see [TUI § Editor](TUI#editor-integration). |
| `L` | Always | Hand off to `lazygit` (when installed). |
| `r` | Always | Refresh trunk + recompute merge-base. |
| `q` / `Esc` | Always | Back to dashboard. |

## See also

- [TUI](12-TUI) — the dashboard the diff view is part of, including
  editor + lazygit handoff details.
- [Configuration Reference: `[diff]`](Configuration-Reference#diff).
- [Configuration Reference: `[editor]`](Configuration-Reference#editor)
  for pinning a project editor.
