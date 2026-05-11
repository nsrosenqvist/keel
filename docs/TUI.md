# TUI

Bare `scaffl` (or `scaffl ui` explicitly) opens the embedded
dashboard: a browseable view of every recipe and script, plus
service status panes, output streaming, and a built-in branch-diff
view.

## Layout

- **Sidebar** — every recipe, script, and service. Recipes / scripts
  are listed by name with their `desc`; services show their lifecycle
  state (Running / Stopped) and a backend tag (`compose`, `systemd`,
  `custom`).
- **Main pane** — output for whatever's selected. Selecting a
  recipe and pressing **Enter** launches it; output streams in the
  same pane, color-preserved.
- **Top bar** — project name, current worktree slug + offset, active
  pane shortcut.
- **Footer** — keymap hints relevant to the focused view.

## Auto-discovery

At startup, scaffl asks the active container backend for its service
list (`docker compose config --services`). Every name shows up as a
service row in alphabetical order. Most projects need nothing else —
service rows just appear.

## Explicit panes (`[[ui.pane]]`)

Add explicit `[[ui.pane]]` entries when you want one of:

- Pinned ordering (declared services come first in declaration
  order; auto-discovered services follow alphabetically).
- A `key = "1"` shortcut for direct focus.
- A row that exists *before* compose detection runs (useful on slow
  setups or when compose detection might fail and you still want the
  row visible).

```toml
[[ui.pane]]
type    = "service"
service = "app"
key     = "1"

[[ui.pane]]
type        = "command"
name        = "test"
key         = "t"
restart_on  = ["src/**", "tests/**"]

[[ui.pane]]
type        = "watcher"
glob        = ["src/**"]
on_change   = "test"
debounce_ms = 500
```

`type` is `service`, `command`, or `watcher`. Without one of the
above reasons, omit the block — auto-discovery does the same job
and the redundant declarations rot when the docker-compose.yml
service list changes.

## Keymaps

### Navigation

| Key | Action |
|---|---|
| `↑` / `↓` | Move sidebar selection. |
| `Tab` | Cycle focus (sidebar → main pane → footer). |
| `Enter` | Run / focus the selected entry. |
| `q` | Quit. |

### Service lifecycle (uniform across compose / systemd / custom)

| Key | Action |
|---|---|
| `r` | Start (or restart, depending on context). |
| `R` | Force restart. |
| `s` | Stop. |
| `S` | Stop and remove (compose only). |
| `U` | Up (compose only). |
| `D` | Down (compose only). |

### Views

| Key | Action |
|---|---|
| `G` | Open the [Diff View](./Diff-View.md). |
| `W` | Open the worktree switcher modal. |

## Worktree switcher (`W`)

Modal lists every checkout under the repo and hot-reloads scaffl
into a different worktree without restarting. The "+ new worktree"
entry opens a branch-first picker:

- Type to filter local + remote branches.
- Pick one to attach an existing checkout.
- Or take the **"create branch '<input>' off HEAD"** sentinel for
  `git worktree add -b`.

The path field auto-fills as `<parent>/<slug(branch)>`; Tab into it
for a manual override.

See [Worktrees](./Worktrees.md) for the slug + offset model that
makes per-worktree ports / `COMPOSE_PROJECT_NAME` automatic.

## Pane types

### `service`

Pinned slot for a service row. The lifecycle keymap operates on it.
`key = "1"` adds a single-key focus shortcut.

### `command`

Pinned slot for a recipe / script. Press `Enter` (or the `key`
shortcut) to run it. `restart_on = ["src/**", ...]` re-runs on file
changes — same engine as `scaffl watch`. See [Watch](./Watch.md).

### `watcher`

A long-running watcher pane. `glob = [...]` selects watched files,
`on_change = "<recipe>"` is the recipe to run, `debounce_ms` is the
debounce window (default 300).

## See also

- [Diff View](./Diff-View.md) for the `G` view's trunk resolution.
- [Watch](./Watch.md) for the standalone `scaffl watch` command,
  which uses the same engine as `restart_on` panes.
- [Worktrees](./Worktrees.md) for `W` behavior.
