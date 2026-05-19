# Quick Tour

Five minutes through the major features, in the order you'd
typically meet them. Everything here cross-links to the deeper
docs. Already comfortable? Jump to
[Configuration Reference](16-Configuration-Reference).

## 1. Two ways to define commands

Declaratively in `ampelos.toml`:

```toml
[command.up]
desc = "Start the stack"
run  = "docker compose up -d"
```

Or as a shell script under `.ampelos/commands/`:

```sh
# .ampelos/commands/seed
#!/usr/bin/env bash
# @desc: Seed the database with development data
# @in: app
set -euo pipefail
php artisan migrate:fresh
php artisan db:seed
```

Both show up in `ampelos list`, both are runnable as `ampelos <name>`.
Use whichever shape matches the command's complexity. See
[Recipes and Scripts](03-Recipes-and-Scripts).

## 2. Knows where commands run

`in = "<service>"` execs inside a Docker Compose service after a
status preflight. Absent → host. `tty = true` allocates a pseudo-TTY.

```toml
[command.test]
in           = "app"
run          = "composer test"
forward_args = true        # ampelos test --filter Login → composer test --filter Login
```

Backend selection is `[runtime].backend` — compose, docker,
podman, or none. See [Container Backends](05-Container-Backends).

## 3. Per-worktree isolation, automatic

Two checkouts of the same repo run side-by-side without port or
container collisions:

```toml
[worktrees]
dotenv = ".env"            # auto-write resolved env to .env

[env]
APP_PORT = { base = "8080", offset = "AMPELOS_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "AMPELOS_WORKTREE_OFFSET" }
```

`AMPELOS_WORKTREE_OFFSET` is computed deterministically from the
worktree slug, so each checkout gets a stable, distinct integer
that drifts ports / `COMPOSE_PROJECT_NAME` / anything else
needing isolation. See [Worktrees](08-Worktrees).

## 4. Git hooks, pre-commit-compatible

Native ampelos hooks plus `.pre-commit-config.yaml` repos coexist
behind the same shim:

```toml
[hooks]
pre-commit = ["check:format", "check:lint"]
```

```sh
ampelos hooks install
```

External repos in `.pre-commit-config.yaml` are cloned into
`.ampelos/cache/hooks/<rev>/` and run natively — no `pre-commit`
binary required. See [Hooks](09-Hooks).

## 5. First-time setup with `ampelos install`

Drop ordered shell files into `.ampelos/install/`:

```
.ampelos/install/
  01-copy-env
  02-install-deps
  03-migrate
  04-seed-data        # @optional: yes
```

Run them:

```sh
ampelos install
```

Each step runs in order, with a line-redraw progress UI. Failures
prompt **"Resume from `<step>`?"** on the next run. Marking a step
`# @optional: yes` lets it fail without halting the rest;
`# @interactive: yes` hands the terminal to the step so
[`ampelos lib *`](15-Shell-Library) prompts work. See
[Install Flow](11-Install-Flow).

## 6. Agent instructions from upstream repos

Pull `CLAUDE.md`, `AGENTS.md`, and `.claude/skills/` from a shared
upstream:

```toml
[[agents.sources]]
name = "org-baseline"
repo = "https://github.com/your-org/agent-baseline"
rev  = "v1.0.0"
```

```sh
ampelos agents install        # pull pinned upstream
ampelos agents update         # re-resolve revs
ampelos agents status         # per-source rev + per-file drift
```

Whole-file ownership: ampelos tracks every file it writes by
SHA-256 and never touches local sibling files. See
[Agents](10-Agents).

## 7. Open the dashboard

```sh
ampelos
```

A sidebar of recipes / scripts / services, an output pane,
lifecycle keymaps for compose + systemd + custom services, the
built-in [diff view](13-Diff-View) (`G`), and a
[worktree switcher](Worktrees#tui-worktree-switcher-w) (`W`).
See [TUI](12-TUI).

## 8. Watch mode

```sh
ampelos watch test
```

Re-runs the recipe on filesystem change with a 300 ms debounce. See
[Watch](14-Watch).

## 9. Shell prompts in any script

```sh
EMAIL=$(ampelos lib ask "Admin email")
ampelos lib confirm "Seed the DB?" --default yes && php artisan db:seed
SVC=$(ampelos lib select "Service" app db redis)
```

Prompts to stderr, answer to stdout, `--default` for non-tty / CI.
See [Shell Library](15-Shell-Library).

## Where to go next

- [Configuration Reference](16-Configuration-Reference) — every
  key.
- [Commands Reference](17-Commands-Reference) — every CLI flag.
- [Examples](18-Examples) — runnable projects.
