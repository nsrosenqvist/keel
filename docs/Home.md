# ampelos

> *Bends to your project. Holds it together.*

Ampelos is a dev-loop trellis. One `ampelos.toml` holds Docker
Compose, scripts, hooks, and agent instructions together. Your
stack runs the way your project runs it — the tool bends, not
the project.

- **Site:** [ampelos.dev](https://ampelos.dev)
- **Status:** alpha. End-to-end usable on Linux and macOS
  for the features documented here.

## 60 seconds with ampelos

```sh
cargo install --path .   # from a clone
cd my-project
ampelos init                              # generate a starter ampelos.toml
ampelos                                   # open the TUI dashboard
```

A minimal `ampelos.toml`:

```toml
[project]
name = "myapp"

[runtime]
backend         = "compose"
default_service = "app"

[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.test]
desc         = "Run test suite"
needs        = ["up"]
in           = "app"
run          = "composer test"
forward_args = true

[hooks]
pre-commit = ["check"]
```

Then:

```sh
ampelos up                   # docker compose up -d
ampelos test --filter Login  # forwards to composer test
ampelos hooks install        # writes .git/hooks/pre-commit
ampelos                      # TUI dashboard with every recipe + service
```

## Where to go next

- **New here?** → [Getting Started](01-Getting-Started): install,
  `ampelos init`, your first recipe and hook.
- **Want the tour?** → [Quick Tour](02-Quick-Tour): a guided
  5-minute walk through recipes, hooks, agents, and the TUI.
- **Need the reference?** →
  [Configuration Reference](16-Configuration-Reference) for every
  `ampelos.toml` key, [Commands Reference](17-Commands-Reference)
  for every CLI subcommand.

## All pages

### Concepts

- [Recipes and Scripts](03-Recipes-and-Scripts) — declarative
  TOML vs `.ampelos/commands/` shell scripts.
- [Environments](04-Environments) — `[env]`, dotenv layering,
  `base + offset` arithmetic.
- [Container Backends](05-Container-Backends) — compose / docker
  / podman / none, passthrough resolution.
- [Non Container Services](07-Non-Container-Services) — system
  daemons alongside compose.
- [Worktrees](08-Worktrees) — slug + offset, per-worktree
  isolation, dotenv writer.

### Subsystems

- [Hooks](09-Hooks) — git hook installer +
  `.pre-commit-config.yaml` runner.
- [Agents](10-Agents) — sync agent instructions / skills from
  upstream repos.
- [Install Flow](11-Install-Flow) — `.ampelos/install/` ordered
  setup steps with state + resume.
- [TUI](12-TUI) — the embedded dashboard.
- [Diff View](13-Diff-View) — branch-review surface pinned to
  the trunk merge-base.
- [Watch](14-Watch) — re-run a recipe on filesystem change.
- [Shell Library](15-Shell-Library) — `ampelos lib ask|confirm|…`
  prompts for shell scripts.

### Reference

- [Configuration Reference](16-Configuration-Reference) — every
  `ampelos.toml` key.
- [Commands Reference](17-Commands-Reference) — every CLI
  subcommand.

### Resources

- [Examples](18-Examples) — runnable ampelos projects under
  `examples/`.
- [Troubleshooting](19-Troubleshooting) — `ampelos doctor`, common
  pitfalls.

### Project

- [Architecture](20-Architecture) — operating principles, crate
  layout.
- [Contributing](21-Contributing) — verification ladder, commit
  conventions, how to send a PR.
