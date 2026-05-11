# Non Container Services

Some projects mix compose-managed containers with services
controlled some other way: a system Postgres, a brew-installed
Redis, an `ngrok` tunnel, an LSP daemon. scaffl handles them through
two config shapes that compile down to the same internal
`CustomBackend`.

## `[[services.custom]]` — generic

You describe how to control it.

```toml
[[services.custom]]
name    = "ngrok"
status  = "pgrep -x ngrok"          # exit 0 = Running; non-zero = Stopped
start   = "ngrok http 8080 > /tmp/ngrok.log 2>&1 &"
stop    = "pkill -x ngrok"
restart = "..."                     # optional; defaults to stop && start
logs    = "tail -f /tmp/ngrok.log"  # optional; absent → no log tailing
desc    = "Public tunnel"           # optional; shown in the TUI
```

Required fields: `name`, `status`, `start`, `stop`. The `status`
command's exit code is the contract — stdout / stderr aren't
parsed.

## `[[services.systemd]]` — sugar

Fills in start / stop / restart / status / logs from `systemctl`.

```toml
[[services.systemd]]
name  = "postgres"
unit  = "postgresql.service"
scope = "user"                      # "user" (default) | "system"
```

The `.service` suffix is conventional but optional — systemctl
accepts both. `scope = "system"` is for shared system daemons; the
default is per-user (`systemctl --user`) since dev-loop services are
almost always user-scoped.

For anything beyond `unit` + `scope` (custom systemctl flags,
drop-in envs), drop down to `[[services.custom]]` and call
`systemctl` from there.

## Naming rules

Service names must be unique across `services.custom`,
`services.systemd`, and the compose project. Collisions error at
config-load time so you find them before they're a debugging
problem.

## TUI integration

Every declared service gets a row in the TUI sidebar tagged with its
backend kind. The lifecycle keymap is uniform across all kinds:

| Key | Action |
|---|---|
| `r` | Start (or restart, depending on context) |
| `R` | Force restart |
| `s` | Stop |
| `S` | Stop and remove (compose only) |
| `U` | Up (compose only) |
| `D` | Down (compose only) |

See [TUI](./TUI.md).

## Backend = "none" stacks

A project with `[containers].backend = "none"` plus only
`[[services.systemd]]` / `[[services.custom]]` declarations is fully
supported:

```toml
[containers]
backend = "none"

[[services.systemd]]
name = "postgres"
unit = "postgresql.service"

[[services.custom]]
name   = "redis"
status = "redis-cli ping > /dev/null"
start  = "brew services start redis"
stop   = "brew services stop redis"
```

scaffl skips compose preflight entirely; `scaffl ui` shows just the
declared services.

## Doctor probe

`scaffl doctor` runs each declared service's `status` command and
reports running / stopped per entry. Use it to validate the
declarations before opening the TUI.

## See also

- [Container Backends](./Container-Backends.md) for how compose
  services interact with the same TUI keymap.
- [Configuration Reference](./Configuration-Reference.md#servicescustom--servicessystemd).
