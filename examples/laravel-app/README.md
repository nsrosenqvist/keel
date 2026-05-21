# laravel-app example

A croft config that mirrors a typical Laravel + Docker Compose dev loop.
This is the shape that croft was specifically designed to replace —
projects where a hand-rolled `dev` shell script grows up over time.

## Surface

```sh
croft                   # TUI dashboard

# Lifecycle
croft up                # docker compose up -d
croft down              # docker compose down
croft observability     # up with the observability profile

# Inside the app container (with TTY)
croft shell             # /bin/sh in the app container
croft artisan migrate   # php artisan migrate
croft composer require pkg
croft tinker

# Database / setup
croft migrate
croft fresh             # migrate:fresh --seed
croft setup             # script: full bootstrap

# Tests (with env overrides + container exec)
croft test
croft test --filter Login

# Quality
croft check             # check:frontend + check:backend
croft check:frontend
croft check:backend

# Local dev with mprocs
croft local

# Hooks
croft hooks install
croft hooks run pre-commit

# Watch mode
croft watch test
```

## Mapping from a hand-rolled `dev` script

| Shell-script idiom | croft equivalent |
| --- | --- |
| `./dev up` | `croft up` |
| `./dev artisan migrate` | `croft artisan migrate` |
| Pre-flight container check | `[command.<x>] in = "app"` (automatic) |
| `case` block in script | `[command.<name>]` recipe |
| Multi-line setup function | Script under `.croft/commands/` |
| `docker compose ps` passthrough | Automatic (compose_passthrough = true) |
| Env files loaded with `source` | `[env_files] files = [...]` |
| `WWWUSER=$(id -u)` | `[env] WWWUSER = { from_command = "id -u" }` |

## Try it

The example is wireable but not executable — there's no actual Laravel
codebase here. To smoke-test parsing only:

```sh
croft --project examples/laravel-app list
croft --project examples/laravel-app which test
croft --project examples/laravel-app --explain artisan
```
