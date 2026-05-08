# laravel-app example

A scaffl config that mirrors a typical Laravel + Docker Compose dev loop.
This is the shape that scaffl was specifically designed to replace —
projects where a hand-rolled `dev` shell script grows up over time.

## Surface

```sh
scaffl                   # TUI dashboard

# Lifecycle
scaffl up                # docker compose up -d
scaffl down              # docker compose down
scaffl observability     # up with the observability profile

# Inside the app container (with TTY)
scaffl shell             # /bin/sh in the app container
scaffl artisan migrate   # php artisan migrate
scaffl composer require pkg
scaffl tinker

# Database / setup
scaffl migrate
scaffl fresh             # migrate:fresh --seed
scaffl setup             # script: full bootstrap

# Tests (with env overrides + container exec)
scaffl test
scaffl test --filter Login

# Quality
scaffl check             # check:frontend + check:backend
scaffl check:frontend
scaffl check:backend

# Local dev with mprocs
scaffl local

# Hooks
scaffl hooks install
scaffl hooks run pre-commit

# Watch mode
scaffl watch test
```

## Mapping from a hand-rolled `dev` script

| Shell-script idiom | scaffl equivalent |
| --- | --- |
| `./dev up` | `scaffl up` |
| `./dev artisan migrate` | `scaffl artisan migrate` |
| Pre-flight container check | `[command.<x>] in = "app"` (automatic) |
| `case` block in script | `[command.<name>]` recipe |
| Multi-line setup function | Script under `.scaffl/commands/` |
| `docker compose ps` passthrough | Automatic (compose_passthrough = true) |
| Env files loaded with `source` | `[env_files] files = [...]` |
| `WWWUSER=$(id -u)` | `[env] WWWUSER = { from_command = "id -u" }` |

## Try it

The example is wireable but not executable — there's no actual Laravel
codebase here. To smoke-test parsing only:

```sh
scaffl --project examples/laravel-app list
scaffl --project examples/laravel-app which test
scaffl --project examples/laravel-app --explain artisan
```
