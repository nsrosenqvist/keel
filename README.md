# ampelos

> *Bends to your project. Holds it together.*

Ampelos is a dev-loop trellis. One `ampelos.toml` holds Docker
Compose, scripts, hooks, and agent instructions together — so
your stack runs the way *your project* runs it, not the way a
tool demands.

Write commands declaratively in `ampelos.toml` or as shell scripts
under `.ampelos/commands/`. Wrap Docker Compose, run host tooling,
install git hooks that natively run a `.pre-commit-config.yaml`
subset, sync agent instructions from upstream repos, and
supervise the whole stack from a built-in TUI.

- **Site:** [ampelos.dev](https://ampelos.dev)
- **Status:** alpha. End-to-end usable on Linux and macOS
  for the features documented in the wiki.

## Install

Homebrew (macOS / Linux):

```sh
brew install nsrosenqvist/ampelos/ampelos
```

From a clone (any platform):

```sh
cargo install --path .
```

Once published to crates.io:

```sh
cargo install ampelos
```

The binary is named `ampelos`.

## 60 seconds with ampelos

```toml
# ampelos.toml
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
ampelos up                   # docker compose up -d
ampelos test --filter Login  # forwards to composer test
ampelos hooks install        # writes .git/hooks/pre-commit
ampelos                      # TUI dashboard
```

Anything not matched as a recipe / script / built-in falls through
to `docker compose <cmd>` (`ampelos ps`, `ampelos logs app`, …) or, if
`<cmd>` is a compose service, to `docker compose exec <cmd>` (so
`ampelos app php -v` works).

## Documentation lives in the wiki

The full feature set, configuration reference, command reference,
subsystem deep-dives, and onboarding flow live in the
[**ampelos wiki**](https://github.com/nsrosenqvist/ampelos/wiki).
The wiki is auto-synced from
[`docs/`](./docs/) on every push to `main`, so the source-tree copy
is always the canonical reference.

Start here:

- [**Home**](https://github.com/nsrosenqvist/ampelos/wiki/Home) — landing + page index.
- [**Getting Started**](https://github.com/nsrosenqvist/ampelos/wiki/Getting-Started) — first project, first recipe, first hook.
- [**Quick Tour**](https://github.com/nsrosenqvist/ampelos/wiki/Quick-Tour) — guided walk through every major feature.
- [**Configuration Reference**](https://github.com/nsrosenqvist/ampelos/wiki/Configuration-Reference) — every `ampelos.toml` key.
- [**Commands Reference**](https://github.com/nsrosenqvist/ampelos/wiki/Commands-Reference) — every CLI subcommand.
- [**Examples**](https://github.com/nsrosenqvist/ampelos/wiki/Examples) — runnable ampelos projects.
- [**Troubleshooting**](https://github.com/nsrosenqvist/ampelos/wiki/Troubleshooting) — `ampelos doctor` and common pitfalls.

## Examples (in this repo)

- [`examples/minimal`](./examples/minimal/) — smallest useful config.
- [`examples/laravel-app`](./examples/laravel-app/) — Laravel + Docker
  Compose, modeled on what ampelos was built to replace.
- [`examples/install-flow`](./examples/install-flow/) — `.ampelos/install/`
  ordered setup with optional + interactive steps.
- [`examples/hooks`](./examples/hooks/) — native `[hooks]` plus a
  `.pre-commit-config.yaml` mixing local and external repos.
- [`examples/agents-upstream`](./examples/agents-upstream/) — sample
  upstream layout for sharing agent instructions across repos.

## Contributing

See [Contributing](https://github.com/nsrosenqvist/ampelos/wiki/Contributing)
in the wiki for the verification ladder, Conventional Commits
conventions, and how to add a new example. AI agents working on this
repo: read [`AGENTS.md`](./AGENTS.md).

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
