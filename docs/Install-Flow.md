# Install Flow

`keel install` is the project-bootstrap surface: a teammate
clones the repo, runs one command, and gets a working dev
environment. You define an ordered list of steps once; keel runs
them with progress display, remembers what succeeded, and resumes
where it left off if something fails.

## Quickstart

**1. Drop a step file** in `.keel/install/`:

```sh
# .keel/install/10-deps.sh
#!/usr/bin/env bash
# @desc: Install dependencies
set -euo pipefail
composer install
npm ci
```

Numeric prefix sets order; the description shows in the
progress display.

**2. Run it:**

```sh
keel install
```

Each step shows a row that updates in place (spinner → ✓ / ✗,
plus duration). Tail output for the active step shows below its
row. Re-running is safe — successful steps don't re-execute
unless you ask.

**3. Add more steps as the project grows:**

```
.keel/install/
├── 10-deps.sh
├── 20-db-up.sh
├── 30-migrate.sh
└── 40-seed.sh
```

That's it. Hook shims and agent files are installed automatically
as synthetic steps around your plan (see below).

## Mental model

- **Two ways to define a step.** A file under `.keel/install/`
  (most common), or a `[install].steps = [...]` entry in
  `keel.toml` referencing a recipe or inline command. Files and
  the config list are alternatives, not layers — when
  `[install].steps` is set, it wins.
- **Two synthetic steps wrap your plan.** keel auto-appends
  `apply-agents` at the start (if `[agents]` is declared) and
  `install-hooks` at the end. You don't write or order these —
  they're managed for you.
- **State remembers progress.** Every step's last outcome is
  recorded in `.keel/install.state.json`. On the next
  `keel install`, if any step is unresolved, keel prompts
  "Resume from `<step>`?" so a half-finished setup doesn't redo
  the slow parts.

## Common tasks

### Add a step

Drop a shell file in `.keel/install/`. Numeric prefix sets
order:

```sh
# .keel/install/25-rebuild-search.sh
#!/usr/bin/env bash
# @desc: Rebuild search index
set -euo pipefail
php artisan scout:import
```

Hidden files (`.foo`) and files starting with `_` are skipped, so
helper scripts can live alongside.

### Run a step inside a service container

Set `@in: <service>` (same key as recipes):

```sh
# .keel/install/20-composer.sh
#!/usr/bin/env bash
# @desc: Install PHP deps
# @in: app
set -euo pipefail
composer install
```

`@in` routes through the configured `[runtime].backend`. Devcontainer
users get the devcontainer automatically for steps without `@in`.

### Prompt for input

Mark the step interactive so it gets the terminal:

```sh
# .keel/install/50-secrets.sh
#!/usr/bin/env bash
# @desc: Configure first-run secrets
# @interactive: yes
EMAIL=$(keel lib ask "Admin email")
PASS=$(keel lib password "Admin password")
echo "ADMIN_EMAIL=$EMAIL" >> .env
echo "ADMIN_PASS=$PASS"   >> .env
```

`keel lib ask | confirm | password | select | filter` give you
prompt helpers without an extra dependency. See [Shell Library](Shell-Library).

### Mark a step optional

```sh
# .keel/install/60-fixtures.sh
#!/usr/bin/env bash
# @desc: Pull the latest fixtures
# @optional: yes
make pull-fixtures
```

A non-zero exit is recorded as `skipped`, not `failed`. The plan
continues. Use for nice-to-haves that might fail in some
environments (no network access, missing credentials, etc.).

### Run a single step

```sh
keel install 30-migrate
```

Useful when a maintainer adds a new step that every teammate
needs to apply ("everyone please run `keel install
rebuild-search-index` once"). Updates only that step's state
record.

### Resume after a failure

If a step fails, keel exits non-zero and records `failed`. Fix
the underlying issue and re-run:

```sh
keel install              # prompts "Resume from <step>?"
keel install --resume     # non-interactive resume
keel install --restart    # wipe state and start over
```

In CI / piped invocations, the resume prompt defaults to "yes" so
non-interactive runs do the right thing.

### Preview the plan without running

```sh
keel install --dry-run    # print steps, don't execute
keel install --list       # plan + last-known status per step
```

### Refresh hook caches

```sh
keel install --update-hooks
```

Forces a re-clone of every external repo in
`.pre-commit-config.yaml`. Useful when an upstream moves a tag.

### Order steps from `keel.toml` instead

If your steps are mostly recipes you already have, declare them
directly:

```toml
[install]
steps = ["copy-env", "deps", "db:migrate", { name = "seed", optional = true }]
```

Each entry is either a recipe / script name or an inline table
with `{ name, run, in, env, cwd, optional, interactive }`.
`[install].steps` is the plan; `.keel/install/` files are
ignored when it's set.

## Reference

### Step frontmatter

For shell-file steps, the optional `# @key: value` block at the
top (terminated by the first non-`# @` line) sets the same
fields as `[command.*]` recipes plus a couple of install-only
ones:

| Key | Notes |
|---|---|
| `@desc` | One-line description, shown in the renderer. |
| `@in` | Service to exec inside (compose backend). Absent → host. |
| `@tty` | Allocate a TTY. |
| `@env` | Comma-separated `K=V` pairs added to the step env. |
| `@cwd` | Working directory. Relative to project root. |
| `@optional` | `true` → non-zero exit is `skipped`, not `failed`. |
| `@interactive` | `true` hands the terminal to the step. |

### Synthetic steps

Two steps are auto-appended:

- **`apply-agents`** — runs first, when
  `[agents].install_with_setup = true` (default) and at least one
  `[[agents.sources]]` is declared. See [Agents](Agents).
- **`install-hooks`** — runs last, when
  `[install].install_git_hooks = true` (default). Installs git
  hook shims and prefetches `.pre-commit-config.yaml` external
  repos into `.keel/cache/hooks/<rev>/`. See [Hooks](Hooks).

### Step plan resolution

In precedence:

1. **`[install].steps = [...]` in `keel.toml`.** Each entry is a
   name (recipe / discovered file) or an inline `{ name, run, in,
   env, cwd, optional, interactive }` table.
2. **Files discovered under `.keel/install/`.** Same `# @key:`
   parser as `.keel/commands/`. When `[install].steps` is empty,
   discovered files **are** the plan, file-name sorted.

Install steps are intentionally separate from `keel list` /
`.keel/commands/` — they don't surface in the TUI sidebar or the
command resolver.

### Environment variables

Each step runs with `KEEL_PROJECT_DIR` set to the host path of
the worktree project root. Script-source steps also get
`KEEL_SCRIPT_DIR` pointing at the parent directory of the step
file (typically `.keel/install/`). Both sit alongside the
resolved [Environments](Environments) layers and `@env:`
declarations. Inline install steps in `[install].steps` get
`KEEL_PROJECT_DIR` only — there's no script file for
`KEEL_SCRIPT_DIR` to point at.

### Renderer

A small line-redraw printer (not a TUI). Each step gets a row
that updates in place; the active step's tail output (last 3
lines) shows below its row:

```
✓ 01-copy-env                    (12 ms)
✓ 02-install-deps                (3.4 s)
◐ 03-migrate                     (running)
    Running migration 2026_05_10_create_users_table
    Running migration 2026_05_11_create_sessions_table
```

### CLI flags

| Flag | Notes |
|---|---|
| `--resume` | Non-interactive resume from the first unresolved step. |
| `--restart` | Wipe state, run from step one. |
| `--dry-run` | Print the plan without executing. |
| `--list` | Plan + last-known status per step. |
| `--update-hooks` | Force-refresh the external hook cache. |

### `.keel/.gitignore`

`keel install` writes a marker-delimited managed block in
`.keel/.gitignore` covering `local.toml`, `worktrees/`, `cache/`,
`install.state.json`, and `agents.state.json`. Idempotent —
when the file is already correct, the mtime stays put. Path is
configurable via `[install].gitignore = "..."`.

## See also

- [`examples/install-flow/`](https://github.com/nsrosenqvist/keel/tree/main/examples/install-flow)
  — runnable demo with ordered + optional + interactive steps.
- [Hooks](Hooks) and [Agents](Agents) — the synthetic steps that
  bookend your plan.
- [Shell Library](Shell-Library) — prompts inside interactive
  steps.
