# laravel-app example

A keel config that mirrors a typical Laravel + Docker Compose dev loop.
This is the shape that keel was specifically designed to replace —
projects where a hand-rolled `dev` shell script grows up over time.

## Surface

```sh
keel                   # TUI dashboard

# Lifecycle
keel up                # docker compose up -d
keel down              # docker compose down
keel observability     # up with the observability profile

# Inside the app container (with TTY)
keel shell             # /bin/sh in the app container
keel artisan migrate   # php artisan migrate
keel composer require pkg
keel tinker

# Database / setup
keel migrate
keel fresh             # migrate:fresh --seed
keel setup             # script: full bootstrap

# Tests (with env overrides + container exec)
keel test
keel test --filter Login

# Quality
keel check             # check:frontend + check:backend
keel check:frontend
keel check:backend

# Local dev with mprocs
keel local

# Hooks
keel hooks install
keel hooks run pre-commit

# Watch mode
keel watch test
```

## Mapping from a hand-rolled `dev` script

| Shell-script idiom | keel equivalent |
| --- | --- |
| `./dev up` | `keel up` |
| `./dev artisan migrate` | `keel artisan migrate` |
| Pre-flight container check | `[command.<x>] in = "app"` (automatic) |
| `case` block in script | `[command.<name>]` recipe |
| Multi-line setup function | Script under `.keel/commands/` |
| `docker compose ps` passthrough | Automatic (compose_passthrough = true) |
| Env files loaded with `source` | `[env_files] files = [...]` |
| `WWWUSER=$(id -u)` | `[env] WWWUSER = { from_command = "id -u" }` |

## Try it

The example is wireable but not executable — there's no actual Laravel
codebase here. To smoke-test parsing only:

```sh
keel --project examples/laravel-app list
keel --project examples/laravel-app which test
keel --project examples/laravel-app --explain artisan
```
