# Container Backends

If your project uses Docker Compose, ampelos works out of the box —
detect services, exec into them, run `compose ps` / `logs` /
`build` through `ampelos`. If you use plain Docker, Podman, or no
containers at all, flip one setting in `ampelos.toml` and keep
going. The recipe surface (`in = "<service>"`, lifecycle keymaps,
etc.) doesn't change.

## Quickstart

For a typical compose project, you don't need any config — ampelos
picks Compose by default and auto-discovers your services.

```sh
ampelos ps              # → docker compose ps
ampelos logs app        # → docker compose logs app
ampelos app php -v      # → docker compose exec app php -v
```

Routing a recipe into a service is one line:

```toml
[command.test]
in   = "app"
run  = "composer test"
```

`ampelos test` runs `composer test` inside the `app` service after
a status preflight.

## Mental model

- **Pick a backend.** `[runtime].backend` is one of `compose`
  (default), `docker`, `podman`, or `none`. Everything below
  this layer talks through the same `Backend` trait, so recipe
  semantics are identical regardless.
- **Route per recipe.** `in = "<service>"` says "exec this inside
  that container service." Without it, the recipe runs on the
  host (or inside the devcontainer if that's enabled — see
  [Devcontainer](Devcontainer)).
- **Passthrough fills the gaps.** If you type `ampelos <name>` and
  the name isn't a recipe / script, ampelos tries it as a compose
  subcommand (`docker compose <name>`) and then as a service exec
  shortcut (`docker compose exec <name>`). Both are on by
  default; either can be turned off when they collide with
  recipe names.

## Common tasks

### Route a recipe into a service

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

### Default everything to one service

```toml
[runtime]
default_service = "app"

[command.artisan]
run          = "php artisan"
forward_args = true
# no `in =` — uses default_service = "app"
```

Recipes without explicit `in =` fall back to `default_service`.

### Run an ad-hoc command in a service

Service passthrough lets you skip writing a recipe for one-offs:

```sh
ampelos app php -v      # → docker compose exec app php -v
ampelos db psql -U pg   # → docker compose exec db psql -U pg
```

### Switch to podman or plain docker

```toml
[runtime]
backend = "podman"     # or "docker"
```

Same recipe surface, different CLI underneath. Podman's compose
compatibility layer handles the same `[command.*] in =` routing.

### Work without containers entirely

```toml
[runtime]
backend = "none"
```

Compose preflight is skipped. Useful when `[[services.custom]]`
or `[[services.systemd]]` covers your stack — ampelos still manages
their lifecycle and shows them in the TUI. See
[Non-Container Services](Non-Container-Services).

### Stop a recipe name from being shadowed

If you have a recipe called `up` and don't want `ampelos up` to fall
through to `docker compose up`:

```toml
[runtime]
compose_passthrough = false
```

Same for service-name shortcuts:

```toml
[runtime]
service_passthrough = false
```

### Verify the backend is reachable

```sh
ampelos doctor
```

Reports the active backend, whether its CLI is on `PATH`, and
whether each declared `default_service` exists in the compose
project.

## Reference

### Supported backends

| Backend | Notes |
|---|---|
| `compose` | Default. `docker compose` (v2). Service discovery via `docker compose config --services`. |
| `docker` | Plain `docker` for run / exec. |
| `podman` | Podman compatibility layer. |
| `none` | Skip container preflight entirely; useful when only `[[services.custom]]` / `[[services.systemd]]` apply. |

### Configuration

```toml
[runtime]
backend             = "compose"
default_service     = "app"
compose_passthrough = true
service_passthrough = true
```

- `default_service` — recipes without `in =` use this.
- `compose_passthrough` — when on, `ampelos <name>` falls through to
  `docker compose <name>` for unknown names. Set `false` to make
  recipe names absolute.
- `service_passthrough` — when on, `ampelos <name>` falls through to
  `docker compose exec <name>` if `<name>` matches a compose
  service. Set `false` to make service names absolute.

### TUI integration

The TUI sidebar auto-discovers every compose service and shows
them with the standard lifecycle keymap (`r` start, `R` restart,
`s` stop, `S` stop & remove, `U` up, `D` down). The same keymap
applies to `[[services.custom]]` and `[[services.systemd]]`
declarations, so the user experience is uniform across backends.
See [TUI](TUI).

## See also

- [Non-Container Services](Non-Container-Services) for system
  daemons you want to manage alongside compose.
- [Recipes and Scripts](Recipes-and-Scripts) for the full
  resolution order and `in = "..."` semantics.
- [Devcontainer](Devcontainer) for routing host-targeted recipes
  into a devcontainer workspace.
