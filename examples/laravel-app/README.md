# laravel-app example

A ampelos config that mirrors a typical Laravel + Docker Compose dev loop.
This is the shape that ampelos was specifically designed to replace —
projects where a hand-rolled `dev` shell script grows up over time.

## Surface

```sh
ampelos                   # TUI dashboard

# Lifecycle
ampelos up                # docker compose up -d
ampelos down              # docker compose down
ampelos observability     # up with the observability profile

# Inside the app container (with TTY)
ampelos shell             # /bin/sh in the app container
ampelos artisan migrate   # php artisan migrate
ampelos composer require pkg
ampelos tinker

# Database / setup
ampelos migrate
ampelos fresh             # migrate:fresh --seed
ampelos setup             # script: full bootstrap

# Tests (with env overrides + container exec)
ampelos test
ampelos test --filter Login

# Quality
ampelos check             # check:frontend + check:backend
ampelos check:frontend
ampelos check:backend

# Local dev with mprocs
ampelos local

# Hooks
ampelos hooks install
ampelos hooks run pre-commit

# Watch mode
ampelos watch test
```

## Mapping from a hand-rolled `dev` script

| Shell-script idiom | ampelos equivalent |
| --- | --- |
| `./dev up` | `ampelos up` |
| `./dev artisan migrate` | `ampelos artisan migrate` |
| Pre-flight container check | `[command.<x>] in = "app"` (automatic) |
| `case` block in script | `[command.<name>]` recipe |
| Multi-line setup function | Script under `.ampelos/commands/` |
| `docker compose ps` passthrough | Automatic (compose_passthrough = true) |
| Env files loaded with `source` | `[env_files] files = [...]` |
| `WWWUSER=$(id -u)` | `[env] WWWUSER = { from_command = "id -u" }` |

## Try it

The example is wireable but not executable — there's no actual Laravel
codebase here. To smoke-test parsing only:

```sh
ampelos --project examples/laravel-app list
ampelos --project examples/laravel-app which test
ampelos --project examples/laravel-app --explain artisan
```
