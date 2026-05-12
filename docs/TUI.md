# TUI

Bare `keel` (or `keel ui` explicitly) opens the embedded
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

At startup, keel asks the active container backend for its service
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
| `G` | Open the [Diff View](Diff-View). |
| `W` | Open the worktree switcher modal. |
| `E` | Open the worktree root in your preferred editor (only when the editor handles directories — see [Editor integration](#editor-integration)). |

## Worktree switcher (`W`)

Modal lists every checkout under the repo and hot-reloads keel
into a different worktree without restarting. The "+ new worktree"
entry opens a branch-first picker:

- Type to filter local + remote branches.
- Pick one to attach an existing checkout.
- Or take the **"create branch '<input>' off HEAD"** sentinel for
  `git worktree add -b`.

The path field auto-fills as `<parent>/<slug(branch)>`; Tab into it
for a manual override.

See [Worktrees](Worktrees) for the slug + offset model that
makes per-worktree ports / `COMPOSE_PROJECT_NAME` automatic.

## Pane types

### `service`

Pinned slot for a service row. The lifecycle keymap operates on it.
`key = "1"` adds a single-key focus shortcut.

### `command`

Pinned slot for a recipe / script. Press `Enter` (or the `key`
shortcut) to run it. `restart_on = ["src/**", ...]` re-runs on file
changes — same engine as `keel watch`. See [Watch](Watch).

### `watcher`

A long-running watcher pane. `glob = [...]` selects watched files,
`on_change = "<recipe>"` is the recipe to run, `debounce_ms` is the
debounce window (default 300).

## Editor integration

Two keybinds open the user's preferred editor without leaving keel:

- **`E`** (global) — open the worktree root in the editor. Designed
  for IDEs that take a directory (VS Code, Cursor, JetBrains).
- **`e`** in the diff view (files focus) — open the selected file.

### Resolution

The editor is resolved once at TUI startup, in this order:

1. `[editor] command = "..."` in `keel.toml` (project-pinned).
2. `$VISUAL`.
3. `$EDITOR`.
4. `vim` as a final fallback.

The command string is whitespace-tokenised: `"code --wait"` becomes
`code` with `--wait` prepended on every launch.

### Terminal vs GUI

Terminal editors (vim, nvim, nano, helix, …) suspend the TUI like
the lazygit handoff and resume on exit. GUI editors (code, cursor,
gvim, IntelliJ, …) spawn detached — the TUI stays painted.

### Directory support (the global `E`)

Whether `E` (open worktree root) is offered is an independent
question from terminal-vs-GUI. The registry tags each editor:

- **Opens directories**: vim, nvim, emacs, helix, micro, code,
  cursor, IntelliJ family, subl, kate, zed, gvim, mvim, … (vim and
  emacs drop into netrw / dired; IDEs treat the dir as a workspace).
- **Files only**: nano, kak, mcedit, vi, gedit.

Unknown editors default to files-only. When the gate is closed,
the legend hides `E` and pressing it anyway flashes an explanation.
`e` (open file) is unaffected and works for both modes.

Override per project:

```toml
[editor]
command          = "my-custom-editor"
terminal         = true   # suspend TUI on launch
opens_directory  = true   # enable the global E
```

Classification uses a built-in registry. For editors keel doesn't
know, set the launch mode explicitly:

```toml
[editor]
command  = "my-custom-editor --gui"
terminal = false   # force GUI mode; omit to use the registry
```

Unknown editors with no override default to **terminal mode** (POSIX
convention for `$EDITOR`).

After a terminal-editor session returns, keel marks the diff stale
and reloads — file changes you made show up immediately.

## Lazygit handoff

Press **`L`** in the [Diff View](Diff-View) (or set up a different
trigger) to hand the terminal to `lazygit`. keel leaves the alternate
screen, runs lazygit foreground, and re-enters when you `q` out.
Commits / stages / resets done inside lazygit invalidate the cached
diff — keel reloads automatically.

The `L` keybind is a no-op (with a hint flashed in the status bar)
when `lazygit` isn't on `PATH`.

## See also

- [Diff View](Diff-View) for the `G` view's trunk resolution.
- [Watch](Watch) for the standalone `keel watch` command,
  which uses the same engine as `restart_on` panes.
- [Worktrees](Worktrees) for `W` behavior.
- [Configuration Reference: `[editor]`](Configuration-Reference#editor).
