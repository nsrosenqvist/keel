# scaffl

A dev-loop wrapper that adapts to your project, instead of forcing your
project to adapt to it. Write commands declaratively in `scaffl.toml`, as
shell scripts under `.scaffl/commands/`, or both. Wrap Docker Compose, run
host tooling, install pre-commit-compatible git hooks, and supervise the
whole stack from a built-in TUI.

> **Status:** pre-alpha. Useable end-to-end on Linux and macOS for the
> features below.

## What it does

- **Two ways to define commands.** Recipes in `scaffl.toml` for the simple
  cases; scripts under `.scaffl/commands/` (with optional `# @key:`
  frontmatter) for anything that grows past one line.
- **Knows where commands run.** `in = "<service>"` execs inside a Docker
  Compose service after a status preflight; otherwise commands run on the
  host.
- **Resolves environment in layers.** Inherited process env → `.env` files
  with `${VAR}` expansion → `[env]` specs (with `value` / `default` /
  `from_command` / `required`) → recipe overrides.
- **Composable steps.** `run = ["a", "b"]` runs sequentially; recipe
  references inside the array dispatch through the same engine; `parallel
  = true` runs steps concurrently.
- **TUI dashboard.** Bare `scaffl` opens a browseable view of every recipe
  and script. Press Enter on a host recipe to launch it; output streams
  into a dedicated pane.
- **Diff view (`G`).** A built-in branch-review surface anchored to your
  trunk's merge-base — files changed since you diverged from `main`,
  not just the working-tree-vs-last-commit slice. Trunk is auto-detected
  (`origin/HEAD` → local `main` / `master` / `develop`); pin it
  explicitly with `[diff] base = "release/stable"` in `scaffl.toml`
  if your project uses a non-conventional name.
- **Worktree switcher (`W`).** Modal lists every checkout under the
  repo and hot-reloads scaffl into a different worktree without
  restarting. The "+ new worktree" entry opens a branch-first
  picker: type to filter local + remote branches, pick one to
  attach, or take the "create branch '<input>' off HEAD" sentinel
  for `-b`. The path field auto-fills as `<parent>/<slug(branch)>`
  but Tab into it for a manual override.
- **Watch mode.** `scaffl watch <recipe>` re-runs the recipe whenever
  watched files change, with a debounce window.
- **Git hooks, pre-commit-compatible.** `scaffl hooks install` writes a
  shim that delegates to `scaffl hooks run`. Hooks come from
  `scaffl.toml` `[hooks.<stage>]` and from `.pre-commit-config.yaml`
  (`repo: local` + `language: system | script` runs natively;
  everything else bridges to the `pre-commit` binary if installed).
- **Doctor + init.** `scaffl init` scaffolds a starter `scaffl.toml`
  with detection hints (compose / .env / package.json / composer.json).
  `scaffl doctor` validates backend, env files, and dependency graph.
- **Non-container services alongside compose.** Declare a system
  Postgres or any shell-controllable daemon with
  `[[services.systemd]]` (sugar over `unit` + `scope`) or
  `[[services.custom]]` (you supply status / start / stop /
  restart / logs). They appear in the TUI sidebar with a small
  `systemd` / `custom` badge and respond to the same r / R / s /
  S / U / D keymap as compose services.
- **Worktree-aware envs.** Each git worktree gets a deterministic
  slug + integer offset. `[env]` entries with `base = "8080", offset
  = "SCAFFL_WORKTREE_OFFSET"` make ports vary per worktree
  automatically. `COMPOSE_PROJECT_NAME` is auto-set so two checkouts
  of the same project can run side-by-side. Pin specific worktrees
  with `scaffl worktree assign <slug> <n>`. Set `[worktrees] dotenv =
  ".env"` and the resolved values land in `.env` (idempotent
  managed block) on every scaffl invocation, and `scaffl hooks
  install` auto-wires `post-checkout` / `post-merge` so the file
  stays fresh even when you run `docker compose up` directly.
- **First-time setup with `scaffl install`.** Drop shell files into
  `.scaffl/install/` (numeric prefixes order them: `01-copy-env`,
  `02-composer-install`, …) and `scaffl install` runs them in order
  with a line-redraw progress UI, success-gated between steps. The
  outcome of each step is recorded in `.scaffl/install.state.json`,
  so re-running after a failure prompts "Resume from `<step>`?".
  `scaffl install <step>` runs a single step in isolation
  (migration-style for new teammates). Marking a step `# @optional:
  yes` lets it fail without halting the rest. `# @interactive:
  yes` hands the terminal to the step so `scaffl lib *` prompts
  work. External `.pre-commit-config.yaml` repos are cloned into
  `.scaffl/cache/hooks/<rev>/` and run natively — no dependency on
  the `pre-commit` binary.
- **Shell utilities.** `scaffl lib ask`, `scaffl lib confirm`,
  `scaffl lib password`, `scaffl lib select [--multi]`, and
  `scaffl lib filter` are interactive prompts you can call from
  any shell script. The prompt goes to stderr, the answer to
  stdout — `EMAIL=$(scaffl lib ask "Email")` works as expected.
  Non-tty invocations honour `--default`, so the same scripts
  run cleanly in CI.

## Install

```sh
cargo install --path crates/scaffl-cli   # from a clone
# Or, once published:
# cargo install scaffl-cli
```

The binary is named `scaffl`.

## Quick example

```toml
# scaffl.toml
[project]
name = "myapp"

[containers]
backend = "compose"
default_service = "app"
compose_passthrough = true

[env_files]
files = [".env"]

[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.shell]
desc = "Open a shell in the app container"
in   = "app"
run  = "/bin/sh"
tty  = true

[command.test]
desc         = "Run test suite"
needs        = ["up"]
in           = "app"
run          = "composer test"
forward_args = true

[hooks]
pre-commit = ["check"]
```

```sh
scaffl                      # TUI dashboard
scaffl list                 # table of recipes + scripts
scaffl up                   # docker compose up -d
scaffl shell                # docker compose exec -it app /bin/sh
scaffl test --filter Login  # forwards to composer test
scaffl env                  # print resolved environment
scaffl doctor               # validate backend / deps / env files
scaffl hooks install        # writes .git/hooks/pre-commit
scaffl install              # run .scaffl/install/* + git hooks
scaffl install --list       # show plan + last-known status per step
scaffl install migrate      # re-run a single step (migration-style)
scaffl install --restart    # wipe state, run every step from scratch
scaffl lib ask "Email"      # prompt; answer to stdout (use in scripts)
scaffl lib confirm "Seed?"  # exit 0 = yes, 1 = no
scaffl watch test           # re-run on file changes
scaffl worktree status      # current worktree's slug + offset
scaffl worktree list        # every git worktree + offsets
```

Anything not matched as a recipe / script / built-in falls through to
`docker compose <cmd>` (`scaffl ps`, `scaffl logs app`, ...) or, if
`<cmd>` is a known compose service, to `docker compose exec <cmd>` (so
`scaffl app php -v` works).

## Examples

- [`examples/minimal`](./examples/minimal/) — smallest useful config.
- [`examples/laravel-app`](./examples/laravel-app/) — Laravel + Docker
  Compose, modeled on what scaffl was built to replace.

## Documentation

- [`AGENTS.md`](./AGENTS.md) — operating principles, verification ladder,
  Conventional Commits guidance.

## Layout

```
crates/
  scaffl-cli/        binary; clap; subcommand dispatch
  scaffl-config/     TOML schema, env resolution, script discovery
  scaffl-runtime/    recipe resolver, executor, output sinks
  scaffl-container/  Backend trait; Compose + Null impls
  scaffl-tui/        ratatui dashboard, runner, pane rendering
  scaffl-hooks/      .pre-commit-config.yaml parser, native runner, installer
```

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
