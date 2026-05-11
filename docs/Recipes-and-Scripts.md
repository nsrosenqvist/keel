# Recipes and Scripts

keel gives you two ways to define a command. Use whichever shape
matches the command's complexity — they coexist, neither shadows the
other, and resolution is deterministic.

## Recipes (`[command.<name>]`)

Declarative TOML in `keel.toml`. Best for one-liners and small
sequences.

```toml
[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.test]
desc         = "Run test suite"
needs        = ["up"]
in           = "app"
run          = "composer test"
forward_args = true
```

`needs` runs the listed recipes first; non-zero exit halts the chain.

`run` is either a single string (one shell-parsed command) or an
array of step references. Inside an array, a step is either an inline
shell line or another recipe name — recipe-name steps re-enter the
runtime so `needs` and `in` apply transitively.

```toml
[command.check]
run = ["check:format", "check:lint", "test"]

[command.check.parallel]
parallel = true        # array steps run concurrently
```

`in = "<service>"` execs the command inside a Docker Compose service
after a status preflight. Absent → host. `tty = true` allocates a
TTY (required for shells, irrelevant for pure-stdout commands).

`forward_args = true` appends the trailing CLI args to the command:
`keel test --filter Login` → `composer test --filter Login`.

### Profiles

Named override layers, activated by `--profile <name>`:

```toml
[command.test]
run = "composer test"
tty = true

[command.test.profile.ci]
tty = false
forward_args = true
env = { XDEBUG_MODE = "off" }
```

`keel --profile ci test --testdox` runs `composer test --testdox`
with `tty = false` and the override env.

## Scripts (`.keel/commands/`)

Plain shell files — anything that grows past one or two lines.

```sh
# .keel/commands/seed
#!/usr/bin/env bash
# @desc: Seed the database with development data
# @in: app
# @env: APP_ENV=local
set -euo pipefail
php artisan migrate:fresh
php artisan db:seed
```

keel scans `.keel/commands/` at load time. The optional `# @key:
value` frontmatter (terminated by the first non-`# @` line) sets the
same fields you'd put in a `[command.*]` recipe:

| Key | Equivalent recipe field |
|---|---|
| `@desc` | `desc` |
| `@in` (alias `@service`) | `in` |
| `@tty` | `tty` |
| `@needs` | `needs` (comma-separated) |
| `@env` | `env` (comma-separated `K=V` pairs) |
| `@forward-args` (alias `@forward_args`) | `forward_args` |

Hidden files (`.foo`) and files starting with `_` are skipped. Names
collide with `[command.*]` keys at config-load time and error
explicitly.

## When to pick which

- **Recipe**: 1–3 commands, no flow control, happy in a TOML cell.
- **Script**: anything with conditionals, loops, set-euo-pipefail,
  multiple `if` branches, or that benefits from a real shebang line
  (e.g. `#!/usr/bin/env -S deno run -A`).

Don't mix the two for the same command — recipe-vs-script is a name
collision.

## Resolution order

When you run `keel <name> [args...]` (no explicit subcommand), the
resolver tries:

1. Built-in subcommand (e.g. `list`, `doctor`).
2. `[command.<name>]` recipe.
3. `.keel/commands/<name>` script.
4. `<name>` as a docker-compose subcommand (passthrough), if
   `[runtime].compose_passthrough = true`.
5. `<name>` as a compose service name (exec into it), if
   `[runtime].service_passthrough = true`.

`keel which <name>` prints which slot resolved.

## See also

- [Configuration Reference](./Configuration-Reference.md#commandname-recipes)
  for the recipe schema in full.
- [Commands Reference](./Commands-Reference.md) for the built-in
  subcommands you can't override.
- [Environments](./Environments.md) for how recipe `env` interacts
  with `.env` files and `[env]` arithmetic.
