# keel

> *Your dev loop, held steady.*

Keel is a dev-loop conductor. One `keel.toml` holds Docker
Compose, scripts, hooks, and agent instructions together. Your
stack runs the way your project runs it — the tool bends, not
the project.

- **Site:** [keel.rs](https://keel.rs)
- **Status:** pre-alpha. End-to-end usable on Linux and macOS
  for the features documented here.

## 60 seconds with keel

```sh
cargo install --path .   # from a clone
cd my-project
keel init                              # generate a starter keel.toml
keel                                   # open the TUI dashboard
```

A minimal `keel.toml`:

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
keel up                   # docker compose up -d
keel test --filter Login  # forwards to composer test
keel hooks install        # writes .git/hooks/pre-commit
keel                      # TUI dashboard with every recipe + service
```

## Where to go next

- **New here?** → [Getting Started](Getting-Started): install,
  `keel init`, your first recipe and hook.
- **Want the tour?** → [Quick Tour](Quick-Tour): a guided
  5-minute walk through recipes, hooks, agents, and the TUI.
- **Need the reference?** →
  [Configuration Reference](Configuration-Reference) for every
  `keel.toml` key, [Commands Reference](Commands-Reference)
  for every CLI subcommand.

## All pages

### Concepts

- [Recipes and Scripts](Recipes-and-Scripts) — declarative
  TOML vs `.keel/commands/` shell scripts.
- [Environments](Environments) — `[env]`, dotenv layering,
  `base + offset` arithmetic.
- [Container Backends](Container-Backends) — compose / docker
  / podman / none, passthrough resolution.
- [Non Container Services](Non-Container-Services) — system
  daemons alongside compose.
- [Worktrees](Worktrees) — slug + offset, per-worktree
  isolation, dotenv writer.

### Subsystems

- [Hooks](Hooks) — git hook installer +
  `.pre-commit-config.yaml` runner.
- [Agents](Agents) — sync agent instructions / skills from
  upstream repos.
- [Install Flow](Install-Flow) — `.keel/install/` ordered
  setup steps with state + resume.
- [TUI](TUI) — the embedded dashboard.
- [Diff View](Diff-View) — branch-review surface pinned to
  the trunk merge-base.
- [Watch](Watch) — re-run a recipe on filesystem change.
- [Shell Library](Shell-Library) — `keel lib ask|confirm|…`
  prompts for shell scripts.

### Reference

- [Configuration Reference](Configuration-Reference) — every
  `keel.toml` key.
- [Commands Reference](Commands-Reference) — every CLI
  subcommand.

### Resources

- [Examples](Examples) — runnable keel projects under
  `examples/`.
- [Troubleshooting](Troubleshooting) — `keel doctor`, common
  pitfalls.

### Project

- [Architecture](Architecture) — operating principles, crate
  layout.
- [Contributing](Contributing) — verification ladder, commit
  conventions, how to send a PR.
