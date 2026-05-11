# Container Backends

keel talks to your containers through a Backend abstraction
(`crates/keel-container/`). The backend is selected by
`[containers].backend`; everything else is passthrough.

## Supported backends

| Backend | Notes |
|---|---|
| `compose` | Default. `docker compose` (v2). Service discovery via `docker compose config --services`. |
| `docker` | Plain `docker` for run / exec. |
| `podman` | Podman compatibility layer. |
| `none` | Skip container preflight entirely; useful when only `[[services.custom]]` / `[[services.systemd]]` apply. |

```toml
[containers]
backend             = "compose"
default_service     = "app"
compose_passthrough = true
service_passthrough = true
```

## Per-recipe service routing

`in = "<service>"` execs inside that compose service after a status
preflight. The preflight runs `docker compose ps` and starts the
service if it isn't already up. Absent → host execution.

```toml
[command.shell]
in   = "app"
run  = "/bin/sh"
tty  = true

[command.test]
in   = "app"
run  = "composer test"
```

`tty = true` allocates a TTY (`-it`) — required for interactive
shells, irrelevant for pure-stdout commands.

## `default_service`

When set, recipes without an explicit `in =` field fall back to it:

```toml
[containers]
default_service = "app"

[command.artisan]
run          = "php artisan"
forward_args = true
# implicit `in = "app"`
```

## Passthrough resolution

After recipe / script resolution fails for a name, keel tries:

- **`compose_passthrough = true`** (default): `keel <name>
  [args...]` becomes `docker compose <name> [args...]`. So `keel
  ps`, `keel logs app`, `keel build` all work without writing
  recipes.
- **`service_passthrough = true`** (default): if `<name>` matches a
  compose service, `keel <name> [args...]` becomes `docker compose
  exec <name> [args...]`. So `keel app php -v` execs `php -v`
  inside the `app` service.

Set either to `false` to opt out — useful when a recipe and a service
share a name and you want the recipe to win unconditionally.

## Multi-service stacks

Define one recipe per workflow:

```toml
[command.up]
run = "docker compose up -d"

[command.shell]
in   = "app"
run  = "/bin/sh"
tty  = true

[command.psql]
in   = "db"
run  = "psql -U postgres"
tty  = true
```

The TUI sidebar (`keel ui`) auto-discovers every compose service
and shows them with lifecycle keymaps (`r` start, `R` restart,
`s` stop, `S` stop & remove, `U` up, `D` down). See [TUI](./TUI.md).

## Doctor

`keel doctor` reports the active backend, whether its CLI is on
PATH, and whether each declared `default_service` exists in the
compose project. See [Troubleshooting](./Troubleshooting.md).

## See also

- [Non Container Services](./Non-Container-Services.md) for system
  daemons you want to manage alongside compose.
- [Recipes and Scripts](./Recipes-and-Scripts.md) for the full
  resolution order and `in = "..."` semantics.
