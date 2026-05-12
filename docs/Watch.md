# Watch

`keel watch <recipe>` re-runs `<recipe>` whenever watched files
change.

## Basic usage

```sh
keel watch test
keel watch test --filter Login
keel watch lint --path src --path tests --debounce-ms 500
```

The recipe runs once at start, then on each filesystem event after a
debounce window. Output streams to your terminal as the recipe runs;
the next event interrupts a running invocation only after it finishes
(no mid-flight kill — recipes are atomic).

## Flags

| Flag | Default | Notes |
|---|---|---|
| `--path <PATH>` | project root | Path to watch. Repeat for multiple paths. |
| `--debounce-ms <MS>` | `300` | Coalesce bursts of events into one re-run. |

Trailing args are forwarded to the recipe in the same way `keel
<recipe> args...` would. Add `forward_args = true` to the recipe so
it picks them up:

```toml
[command.test]
run          = "composer test"
forward_args = true
```

## What it watches

Recursive into `--path`, honouring `.gitignore`. The notify backend
(notify v7) picks the best per-OS implementation: inotify on Linux,
FSEvents on macOS, ReadDirectoryChangesW on Windows.

## Use inside the TUI

The TUI's `[[ui.pane]] type = "watcher"` and `restart_on = [...]` on
command panes use the same engine. See [TUI](TUI).

## See also

- [Recipes and Scripts](Recipes-and-Scripts) — `forward_args`
  and recipe basics.
- [TUI](TUI) — watcher panes pinned in the dashboard.
