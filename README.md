# croft

> *Tend your croft.*

Croft is a per-project dev-loop tool. One `croft.toml` declares
Docker Compose services, scripts, git hooks, and agent
instructions for a single project, so your stack runs the way
*your project* runs it — not the way a tool demands.

Declare commands in `croft.toml` or drop shell scripts under
`.croft/commands/`. Croft wraps Docker Compose, runs host tooling,
installs git hooks that natively read a `.pre-commit-config.yaml`
subset, syncs agent instructions from upstream repos, and
supervises the whole stack from a built-in TUI.

- **Site:** [croft.sh](https://croft.sh)
- **Status:** alpha. End-to-end usable on Linux and macOS
  for the features documented in the wiki.

## Install

Homebrew (macOS / Linux):

```sh
brew install nsrosenqvist/croft/croft
```

From a clone (any platform):

```sh
cargo install --path .
```

Once published to crates.io:

```sh
cargo install croft
```

The binary is named `croft`.

## 60 seconds with croft

```toml
# croft.toml
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

```sh
croft up                   # docker compose up -d
croft test --filter Login  # forwards to composer test
croft hooks install        # writes .git/hooks/pre-commit
croft                      # TUI dashboard
```

Anything not matched as a recipe / script / built-in falls through
to `docker compose <cmd>` (`croft ps`, `croft logs app`, …) or, if
`<cmd>` is a compose service, to `docker compose exec <cmd>` (so
`croft app php -v` works).

## Documentation lives in the wiki

The full feature set, configuration reference, command reference,
subsystem deep-dives, and onboarding flow live in the
[**croft wiki**](https://github.com/nsrosenqvist/croft/wiki).
The wiki is auto-synced from
[`docs/`](./docs/) on every push to `main`, so the source-tree copy
is always the canonical reference.

Start here:

- [**Home**](https://github.com/nsrosenqvist/croft/wiki/Home) — landing + page index.
- [**Getting Started**](https://github.com/nsrosenqvist/croft/wiki/Getting-Started) — first project, first recipe, first hook.
- [**Quick Tour**](https://github.com/nsrosenqvist/croft/wiki/Quick-Tour) — guided walk through every major feature.
- [**Configuration Reference**](https://github.com/nsrosenqvist/croft/wiki/Configuration-Reference) — every `croft.toml` key.
- [**Commands Reference**](https://github.com/nsrosenqvist/croft/wiki/Commands-Reference) — every CLI subcommand.
- [**Examples**](https://github.com/nsrosenqvist/croft/wiki/Examples) — runnable croft projects.
- [**Troubleshooting**](https://github.com/nsrosenqvist/croft/wiki/Troubleshooting) — `croft doctor` and common pitfalls.

## Examples (in this repo)

- [`examples/minimal`](./examples/minimal/) — smallest useful config.
- [`examples/laravel-app`](./examples/laravel-app/) — Laravel + Docker
  Compose, modeled on the dev script croft was built to replace.
- [`examples/install-flow`](./examples/install-flow/) — `.croft/install/`
  ordered setup with optional + interactive steps.
- [`examples/hooks`](./examples/hooks/) — native `[hooks]` plus a
  `.pre-commit-config.yaml` mixing local and external repos.
- [`examples/agents-upstream`](./examples/agents-upstream/) — sample
  upstream layout for sharing agent instructions across repos.

## Contributing

See [Contributing](https://github.com/nsrosenqvist/croft/wiki/Contributing)
in the wiki for the verification ladder, Conventional Commits
conventions, and how to add a new example. AI agents working on this
repo: read [`AGENTS.md`](./AGENTS.md).

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
