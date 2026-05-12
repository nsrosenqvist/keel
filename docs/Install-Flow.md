# Install Flow

`keel install` is the project-bootstrap surface: a teammate clones
the repo, runs one command, and gets a working dev environment.

## Step plan

The plan is resolved from two sources, in this precedence:

1. **`[install].steps = [...]` in `keel.toml`.** Each entry is
   either a name (resolves to a `[command.*]` recipe or to a
   discovered step file) or an inline `{ name, run, in, env, cwd,
   optional, interactive }` table.
2. **Files discovered under `.keel/install/`.** Same `# @key:`
   frontmatter parser as `.keel/commands/`, plus install-only keys
   `cwd`, `optional`, `interactive`. When `[install].steps` is
   empty, discovered files **are** the plan (file-name sorted).

Two synthetic steps are appended automatically:

- **`apply-agents`** — runs first, when
  `[agents].install_with_setup = true` (default) and at least one
  `[[agents.sources]]` is declared. See [Agents](Agents).
- **`install-hooks`** — runs last, when `[install].install_git_hooks
  = true` (default). Installs git hook shims and prefetches any
  external `.pre-commit-config.yaml` repos into
  `.keel/cache/hooks/<rev>/`. See [Hooks](Hooks).

Install steps are deliberately separate from `keel list` /
`.keel/commands/` — they don't surface in the TUI sidebar or the
command resolver.

## Step frontmatter

```sh
#!/usr/bin/env bash
# @desc: Install dependencies
# @optional: false
# @interactive: false
# @env: APP_ENV=local
# @in: app
# @cwd: ./backend
set -euo pipefail
composer install
```

| Key | Notes |
|---|---|
| `@desc` | One-line description; shown in the renderer. |
| `@in` | Service to exec inside (compose backend). Absent = host. |
| `@tty` | Allocate a TTY. |
| `@env` | Comma-separated `K=V` pairs added to the step env. |
| `@cwd` | Working directory for the step. Relative to project root. |
| `@optional` | `true` means a non-zero exit is recorded as `skipped`, not `failed`. |
| `@interactive` | `true` hands the terminal to the step (so `keel lib *` prompts work). |

Numeric prefixes order discovered files alphabetically: `01-copy-env`,
`02-composer-install`, `03-migrate`. Hidden files (`.foo`) and files
starting with `_` are skipped.

## State and resume

Every successful or failed step is recorded in
`.keel/install.state.json`. On a subsequent `keel install`:

- If every step is `ok` or `skipped`, the plan re-runs from step 1.
- Otherwise the user is prompted **"Resume from `<step>`?"**.
  `--resume` bypasses the prompt; `--restart` wipes state and starts
  fresh. In non-tty contexts (CI, piped invocations), the answer
  defaults to "yes" so resume is the no-input behaviour.

`keel install <step>` runs one step in isolation and updates only
that step's record. Useful when a maintainer adds a new step that
every teammate needs to apply ("everyone please run `keel install
rebuild-search-index` once").

## Interactive steps

Marking a step `# @interactive: yes` pauses the renderer and
inherits the parent's stdio for the step's duration. That's how
`keel lib ask | confirm | password | select | filter` work inside
install steps without an IPC sentinel protocol — the step really
does have the terminal.

```sh
#!/usr/bin/env bash
# @desc: Configure first-run secrets
# @interactive: yes
EMAIL=$(keel lib ask "Admin email")
echo "ADMIN_EMAIL=$EMAIL" >> .env
```

See [Shell Library](Shell-Library).

## Optional steps

`# @optional: yes` means a non-zero exit is recorded as `skipped`,
not `failed`. The plan continues. Use it for steps that may fail in
some environments but shouldn't block setup (e.g. seeding optional
test data, fetching nice-to-have artifacts).

```sh
#!/usr/bin/env bash
# @desc: Pull the latest fixtures
# @optional: yes
make pull-fixtures
```

## Renderer

A small line-redraw printer (`src/cli/commands/install/renderer.rs`)
— **not** a TUI. Each step gets a row that updates in place
(◐ running with spinner → ✓ ok / ✗ failed / → skipped, plus
duration). The active step's tail output (last 3 lines) shows below
its row.

```
✓ 01-copy-env                    (12 ms)
✓ 02-install-deps                (3.4 s)
◐ 03-migrate                     (running)
    Running migration 2026_05_10_create_users_table
    Running migration 2026_05_11_create_sessions_table
```

## CLI flags

| Flag | Notes |
|---|---|
| `--resume` | Non-interactive resume from the first unresolved step. |
| `--restart` | Wipe state, run from step one. |
| `--dry-run` | Print the plan without executing. |
| `--list` | Plan + last-known status per step. |
| `--update-hooks` | Force-refresh the external hook cache. |

## `.keel/.gitignore`

`keel install` writes a marker-delimited managed block in
`.keel/.gitignore` covering `local.toml`, `worktrees/`, `cache/`,
`install.state.json`, and `agents.state.json`. Idempotent —
re-running install when the file is already correct leaves the mtime
alone. Path is configurable via `[install] gitignore = "..."`. The
shared `src/config/managed_block.rs` helper is the
single implementation of the marker pattern (also used by the
worktree dotenv writer).

## See also

- [`examples/install-flow/`](https://github.com/nsrosenqvist/keel/tree/main/examples/install-flow)
  — runnable demo with ordered + optional + interactive steps.
- [Hooks](Hooks) and [Agents](Agents) for the synthetic
  steps that bookend the user-defined plan.
- [Shell Library](Shell-Library) for prompts inside interactive
  steps.
