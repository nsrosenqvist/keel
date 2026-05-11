# scaffl

A dev-loop wrapper that adapts to your project, instead of forcing
your project to adapt to it. Write commands declaratively in
`scaffl.toml`, as shell scripts under `.scaffl/commands/`, or both.
Wrap Docker Compose, run host tooling, install pre-commit-compatible
git hooks, sync agent instructions and skills from upstream repos,
and supervise the whole stack from a built-in TUI.

> **Status:** pre-alpha. Useable end-to-end on Linux and macOS for
> the features documented in the wiki.

## Install

```sh
cargo install --path crates/scaffl-cli   # from a clone
# Or, once published:
# cargo install scaffl-cli
```

The binary is named `scaffl`.

## 60 seconds with scaffl

```toml
# scaffl.toml
[project]
name = "myapp"

[containers]
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
scaffl up                   # docker compose up -d
scaffl test --filter Login  # forwards to composer test
scaffl hooks install        # writes .git/hooks/pre-commit
scaffl                      # TUI dashboard
```

Anything not matched as a recipe / script / built-in falls through
to `docker compose <cmd>` (`scaffl ps`, `scaffl logs app`, …) or, if
`<cmd>` is a compose service, to `docker compose exec <cmd>` (so
`scaffl app php -v` works).

## Documentation lives in the wiki

The full feature set, configuration reference, command reference,
subsystem deep-dives, and onboarding flow live in the
[**scaffl wiki**](https://github.com/nsrosenqvist/scaffl/wiki).
The wiki is auto-synced from
[`docs/`](./docs/) on every push to `main`, so the source-tree copy
is always the canonical reference.

Start here:

- [**Home**](https://github.com/nsrosenqvist/scaffl/wiki/Home) — landing + page index.
- [**Getting Started**](https://github.com/nsrosenqvist/scaffl/wiki/Getting-Started) — first project, first recipe, first hook.
- [**Quick Tour**](https://github.com/nsrosenqvist/scaffl/wiki/Quick-Tour) — guided walk through every major feature.
- [**Configuration Reference**](https://github.com/nsrosenqvist/scaffl/wiki/Configuration-Reference) — every `scaffl.toml` key.
- [**Commands Reference**](https://github.com/nsrosenqvist/scaffl/wiki/Commands-Reference) — every CLI subcommand.
- [**Examples**](https://github.com/nsrosenqvist/scaffl/wiki/Examples) — runnable scaffl projects.
- [**Troubleshooting**](https://github.com/nsrosenqvist/scaffl/wiki/Troubleshooting) — `scaffl doctor` and common pitfalls.

## Examples (in this repo)

- [`examples/minimal`](./examples/minimal/) — smallest useful config.
- [`examples/laravel-app`](./examples/laravel-app/) — Laravel + Docker
  Compose, modeled on what scaffl was built to replace.
- [`examples/install-flow`](./examples/install-flow/) — `.scaffl/install/`
  ordered setup with optional + interactive steps.
- [`examples/hooks`](./examples/hooks/) — native `[hooks]` plus a
  `.pre-commit-config.yaml` mixing local and external repos.
- [`examples/agents-upstream`](./examples/agents-upstream/) — sample
  upstream layout for sharing agent instructions across repos.

## Contributing

See [Contributing](https://github.com/nsrosenqvist/scaffl/wiki/Contributing)
in the wiki for the verification ladder, Conventional Commits
conventions, and how to add a new example. AI agents working on this
repo: read [`AGENTS.md`](./AGENTS.md).

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
