# Quick Tour

Five minutes through the major features, in the order you'd
typically meet them. Everything here cross-links to the deeper
docs. Already comfortable? Jump to
[Configuration Reference](./Configuration-Reference.md).

## 1. Two ways to define commands

Declaratively in `keel.toml`:

```toml
[command.up]
desc = "Start the stack"
run  = "docker compose up -d"
```

Or as a shell script under `.keel/commands/`:

```sh
# .keel/commands/seed
#!/usr/bin/env bash
# @desc: Seed the database with development data
# @in: app
set -euo pipefail
php artisan migrate:fresh
php artisan db:seed
```

Both show up in `keel list`, both are runnable as `keel <name>`.
Use whichever shape matches the command's complexity. See
[Recipes and Scripts](./Recipes-and-Scripts.md).

## 2. Knows where commands run

`in = "<service>"` execs inside a Docker Compose service after a
status preflight. Absent → host. `tty = true` allocates a pseudo-TTY.

```toml
[command.test]
in           = "app"
run          = "composer test"
forward_args = true        # keel test --filter Login → composer test --filter Login
```

Backend selection is `[runtime].backend` — compose, docker,
podman, or none. See [Container Backends](./Container-Backends.md).

## 3. Per-worktree isolation, automatic

Two checkouts of the same repo run side-by-side without port or
container collisions:

```toml
[worktrees]
dotenv = ".env"            # auto-write resolved env to .env

[env]
APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "KEEL_WORKTREE_OFFSET" }
```

`KEEL_WORKTREE_OFFSET` is computed deterministically from the
worktree slug, so each checkout gets a stable, distinct integer
that drifts ports / `COMPOSE_PROJECT_NAME` / anything else
needing isolation. See [Worktrees](./Worktrees.md).

## 4. Git hooks, pre-commit-compatible

Native keel hooks plus `.pre-commit-config.yaml` repos coexist
behind the same shim:

```toml
[hooks]
pre-commit = ["check:format", "check:lint"]
```

```sh
keel hooks install
```

External repos in `.pre-commit-config.yaml` are cloned into
`.keel/cache/hooks/<rev>/` and run natively — no `pre-commit`
binary required. See [Hooks](./Hooks.md).

## 5. First-time setup with `keel install`

Drop ordered shell files into `.keel/install/`:

```
.keel/install/
  01-copy-env
  02-install-deps
  03-migrate
  04-seed-data        # @optional: yes
```

Run them:

```sh
keel install
```

Each step runs in order, with a line-redraw progress UI. Failures
prompt **"Resume from `<step>`?"** on the next run. Marking a step
`# @optional: yes` lets it fail without halting the rest;
`# @interactive: yes` hands the terminal to the step so
[`keel lib *`](./Shell-Library.md) prompts work. See
[Install Flow](./Install-Flow.md).

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
keel agents install        # pull pinned upstream
keel agents update         # re-resolve revs
keel agents status         # per-source rev + per-file drift
```

Whole-file ownership: keel tracks every file it writes by
SHA-256 and never touches local sibling files. See
[Agents](./Agents.md).

## 7. Open the dashboard

```sh
keel
```

A sidebar of recipes / scripts / services, an output pane,
lifecycle keymaps for compose + systemd + custom services, the
built-in [diff view](./Diff-View.md) (`G`), and a
[worktree switcher](./Worktrees.md#tui-worktree-switcher-w) (`W`).
See [TUI](./TUI.md).

## 8. Watch mode

```sh
keel watch test
```

Re-runs the recipe on filesystem change with a 300 ms debounce. See
[Watch](./Watch.md).

## 9. Shell prompts in any script

```sh
EMAIL=$(keel lib ask "Admin email")
keel lib confirm "Seed the DB?" --default yes && php artisan db:seed
SVC=$(keel lib select "Service" app db redis)
```

Prompts to stderr, answer to stdout, `--default` for non-tty / CI.
See [Shell Library](./Shell-Library.md).

## Where to go next

- [Configuration Reference](./Configuration-Reference.md) — every
  key.
- [Commands Reference](./Commands-Reference.md) — every CLI flag.
- [Examples](./Examples.md) — runnable projects.
