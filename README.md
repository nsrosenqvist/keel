# scaffl

A dev-loop wrapper that adapts to your project. Define commands declaratively in `scaffl.toml`, as scripts under `.scaffl/commands/`, or both. Wrap Docker Compose, run host tooling, install pre-commit-compatible git hooks, and supervise the whole stack from a built-in TUI.

> **Status:** pre-alpha. The design is locked; implementation is in progress.

## Quick example

```toml
# scaffl.toml
[project]
name = "myapp"

[runtime]
backend = "compose"
default_service = "app"
compose_passthrough = true

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
```

```sh
scaffl up           # docker compose up -d
scaffl shell        # docker compose exec -it app /bin/sh
scaffl test --filter Login   # composer test --filter Login (inside `app`)
scaffl                       # opens the TUI dashboard
```

## Documentation

- [`AGENTS.md`](./AGENTS.md) — operating principles, verification, commit conventions.
- Design plan — see the linked plan file in the project root.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
