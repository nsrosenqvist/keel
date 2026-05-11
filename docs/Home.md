# keel

A dev-loop wrapper that adapts to your project, instead of forcing
your project to adapt to it.

Write commands declaratively in `keel.toml`, as shell scripts
under `.keel/commands/`, or both. Wrap Docker Compose, run host
tooling, install pre-commit-compatible git hooks, sync agent
instructions and skills from upstream repos, and supervise the
whole stack from a built-in TUI.

> **Status:** pre-alpha. Useable end-to-end on Linux and macOS for
> the features documented here.

## 60 seconds with keel

```sh
cargo install --path crates/keel-cli   # from a clone
cd my-project
keel init                              # scaffold a starter keel.toml
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

- **New here?** → [Getting Started](./Getting-Started.md): install,
  `keel init`, your first recipe and hook.
- **Want the tour?** → [Quick Tour](./Quick-Tour.md): a guided
  5-minute walk through recipes, hooks, agents, and the TUI.
- **Need the reference?** →
  [Configuration Reference](./Configuration-Reference.md) for every
  `keel.toml` key, [Commands Reference](./Commands-Reference.md)
  for every CLI subcommand.

## All pages

### Concepts

- [Recipes and Scripts](./Recipes-and-Scripts.md) — declarative
  TOML vs `.keel/commands/` shell scripts.
- [Environments](./Environments.md) — `[env]`, dotenv layering,
  `base + offset` arithmetic.
- [Container Backends](./Container-Backends.md) — compose / docker
  / podman / none, passthrough resolution.
- [Non Container Services](./Non-Container-Services.md) — system
  daemons alongside compose.
- [Worktrees](./Worktrees.md) — slug + offset, per-worktree
  isolation, dotenv writer.

### Subsystems

- [Hooks](./Hooks.md) — git hook installer +
  `.pre-commit-config.yaml` runner.
- [Agents](./Agents.md) — sync agent instructions / skills from
  upstream repos.
- [Install Flow](./Install-Flow.md) — `.keel/install/` ordered
  setup steps with state + resume.
- [TUI](./TUI.md) — the embedded dashboard.
- [Diff View](./Diff-View.md) — branch-review surface anchored to
  trunk merge-base.
- [Watch](./Watch.md) — re-run a recipe on filesystem change.
- [Shell Library](./Shell-Library.md) — `keel lib ask|confirm|…`
  prompts for shell scripts.

### Reference

- [Configuration Reference](./Configuration-Reference.md) — every
  `keel.toml` key.
- [Commands Reference](./Commands-Reference.md) — every CLI
  subcommand.

### Resources

- [Examples](./Examples.md) — runnable keel projects under
  `examples/`.
- [Troubleshooting](./Troubleshooting.md) — `keel doctor`, common
  pitfalls.

### Project

- [Architecture](./Architecture.md) — operating principles, crate
  layout.
- [Contributing](./Contributing.md) — verification ladder, commit
  conventions, how to send a PR.
