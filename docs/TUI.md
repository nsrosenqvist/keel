# TUI

Run `ampelos` with no arguments to open the dashboard: every recipe,
script, and service in one sidebar, with output streaming in the
main pane. Most of the time you won't configure anything — services
auto-appear, recipes show up by name, the standard keymaps just
work.

## Quickstart

```sh
ampelos
```

You'll see a sidebar with three groups: services discovered from
`docker compose`, recipes declared in `[command.*]`, and scripts
under `.ampelos/commands/`.

- **`↑` / `↓`** — move the selection.
- **`Enter`** — run a recipe / focus a service.
- **`G`** — open the branch [Diff View](Diff-View).
- **`W`** — switch worktrees.
- **`q`** — quit.

That's enough to use it. The rest of this page covers
customisation and the keys you'll reach for as you go.

## Mental model

- **Sidebar = your work. Main pane = its output.** Selecting a
  recipe and pressing Enter launches it; output streams in the
  same main pane, color-preserved. Selecting a service shows its
  logs and lifecycle state.
- **Keymaps are uniform across backends.** A compose service, a
  systemd unit, and a custom service all respond to the same
  `r` / `s` / `S` keys.
- **Auto-discovery is the default; explicit panes are escape
  hatches.** You only add `[[ui.pane]]` blocks when you want
  pinned ordering, a numeric shortcut, or a watcher pane.

## Common tasks

### Run a recipe

`Enter` on its sidebar row. Output streams; press `Enter` again
to re-run after it finishes.

### Manage a service

Select it in the sidebar and press a lifecycle key:

| Key | Action |
|---|---|
| `r` | Start (or restart, depending on context). |
| `R` | Force restart. |
| `s` | Stop. |
| `S` | Stop and remove (compose only). |
| `U` | Up (compose only). |
| `D` | Down (compose only). |

Same keys for `[[services.systemd]]` and `[[services.custom]]`
backends — see [Non-Container Services](Non-Container-Services).

### See what's changed in your branch

`G` opens the [Diff View](Diff-View) — every file that differs
from your trunk's merge-base, with hunk navigation and edit /
lazygit handoff.

### Switch worktrees without restarting

`W` opens the worktree-switcher modal:

- Lists every checkout under the repo. Pick one to hot-reload
  ampelos into it.
- "+ new worktree" opens a branch-first picker — type to filter
  local + remote branches, pick one to attach, or take the
  **"create branch '<input>' off HEAD"** sentinel for
  `git worktree add -b`.

The path field auto-fills as `<parent>/<slug>`; Tab into it for
a manual override. See [Worktrees](Worktrees).

### Pin a recipe to a single-key shortcut

```toml
[[ui.pane]]
type = "command"
name = "test"
key  = "t"
```

`t` runs `test` from anywhere in the dashboard.

### Re-run a recipe on file changes

```toml
[[ui.pane]]
type        = "command"
name        = "test"
restart_on  = ["src/**", "tests/**"]
```

Same engine as `ampelos watch` — see [Watch](Watch).

### Add a long-running watcher pane

```toml
[[ui.pane]]
type        = "watcher"
glob        = ["src/**"]
on_change   = "test"
debounce_ms = 500
```

The pane lives in the sidebar; the configured recipe fires on
matching changes.

### Open your editor from the TUI

- **`E`** (global) — open the worktree root. For IDEs that take
  a directory (VS Code, Cursor, JetBrains).
- **`e`** in the diff view (files focus) — open the selected
  file.

The editor is resolved at startup: `[editor].command` →
`$VISUAL` → `$EDITOR` → `vim`. See
[editor reference](#editor-integration) below to pin one or
override its launch mode.

### Hand off to lazygit

`L` in the diff view: ampelos leaves the alternate screen, runs
lazygit in the foreground, and re-enters when you `q` out. Diff
is reloaded automatically. No-op (with a status-bar hint) when
lazygit isn't on `PATH`.

## Reference

### Layout

- **Sidebar** — every recipe, script, and service. Recipes /
  scripts show name + `desc`; services show lifecycle state
  (Running / Stopped) and a backend tag (`compose`, `systemd`,
  `custom`).
- **Main pane** — output for whatever's selected.
- **Top bar** — project name, current worktree slug + offset,
  active pane shortcut.
- **Footer** — keymap hints relevant to the focused view.

### Auto-discovery

At startup, ampelos asks the active backend for its service list
(`docker compose config --services` on compose). Every name
becomes a sidebar row in alphabetical order. No config required.

### Explicit panes (`[[ui.pane]]`)

Add explicit blocks when you want one of:

- Pinned ordering — declared services come first in declaration
  order; auto-discovered ones follow alphabetically.
- A `key = "1"` shortcut for direct focus.
- A row that exists *before* compose detection runs (slow
  setups, or you want the row visible even if detection fails).

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

Without one of the above reasons, omit the block — auto-discovery
does the same job and redundant declarations rot when the compose
service list changes.

### Pane types

- **`service`** — pinned slot for a service row. Lifecycle keymap
  applies.
- **`command`** — pinned slot for a recipe / script. `Enter` or
  the `key` shortcut runs it. `restart_on = [...]` re-runs on
  file changes.
- **`watcher`** — long-running watcher pane. `glob = [...]`
  selects files, `on_change = "<recipe>"` is the recipe to run,
  `debounce_ms` is the debounce window (default 300).

### Full keymap

#### Navigation

| Key | Action |
|---|---|
| `↑` / `↓` | Move sidebar selection. |
| `Tab` | Cycle focus (sidebar → main → footer). |
| `Enter` | Run / focus the selected entry. |
| `q` | Quit. |

#### Service lifecycle

| Key | Action |
|---|---|
| `r` | Start (or restart, depending on context). |
| `R` | Force restart. |
| `s` | Stop. |
| `S` | Stop and remove (compose only). |
| `U` | Up (compose only). |
| `D` | Down (compose only). |

#### Views

| Key | Action |
|---|---|
| `G` | Open the [Diff View](Diff-View). |
| `W` | Open the worktree switcher modal. |
| `E` | Open the worktree root in your editor (when the editor handles directories — see below). |

### Editor integration

Two keybinds launch your preferred editor:

- **`E`** (global) — open the worktree root. For IDEs that take
  a directory.
- **`e`** in the diff view (files focus) — open the selected
  file.

**Resolution**, in order:

1. `[editor].command = "..."` in `ampelos.toml` (project-pinned).
2. `$VISUAL`.
3. `$EDITOR`.
4. `vim` (final fallback).

The command string is whitespace-tokenised: `"code --wait"`
becomes `code` with `--wait` prepended on every launch.

**Terminal vs GUI.** Terminal editors (vim, nvim, nano, helix, …)
suspend the TUI and resume on exit. GUI editors (code, cursor,
gvim, IntelliJ, …) spawn detached — the TUI stays painted. The
registry classifies known editors automatically.

**Directory support (the global `E`).** The registry also tags
each editor as "opens directories" (vim, nvim, code, cursor,
IntelliJ, …) or "files only" (nano, kak, mcedit). When the gate
is closed, `E` is hidden from the legend and pressing it flashes
an explanation. `e` (open file) is unaffected and works for both
modes.

Override per project:

```toml
[editor]
command          = "my-custom-editor"
terminal         = true   # suspend TUI on launch
opens_directory  = true   # enable the global E
```

Unknown editors with no override default to **terminal mode**
(POSIX convention for `$EDITOR`). After a terminal-editor session
returns, ampelos marks the diff stale and reloads — file changes
show up immediately.

## See also

- [Diff View](Diff-View) — `G` view's trunk resolution.
- [Watch](Watch) — the standalone `ampelos watch` command uses the
  same engine as `restart_on` panes.
- [Worktrees](Worktrees) — what `W` switches between.
- [Configuration Reference: `[editor]`](Configuration-Reference#editor).
