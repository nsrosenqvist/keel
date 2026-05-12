# Environments

`ampelos` controls the environment variables your commands see. It
loads `.env` files, lets you compute values (e.g. a port that's
unique per git checkout), and forwards a project-declared subset
into container exec sessions. The same model drives recipe runs,
compose preflight, and the optional `.env` writer.

## Quickstart

**1. List your dotenv files** in `ampelos.toml`:

```toml
[env_files]
files = [".env", ".env.local"]
```

`.env.local` overrides `.env`, both override the inherited shell.

**2. Add a computed value:**

```toml
[env]
LOG_LEVEL = { default = "info" }
APP_PORT  = { base = "8080", offset = "AMPELOS_WORKTREE_OFFSET" }
DATABASE_URL = { from_command = "scripts/db-url.sh", required = true }
```

- `default` is used only if no earlier layer set it.
- `base + offset` picks a per-worktree port so two checkouts can
  run side-by-side. See [Worktrees](Worktrees).
- `from_command` runs a shell command and uses its trimmed stdout.

**3. Verify what's resolved:**

```sh
ampelos env
```

Prints every key as `KEY=VALUE` lines.

## Mental model

- **Layers, later wins.** The merge order is fixed: process env →
  `[env_files]` dotenvs → `[env]` table → recipe `env =`
  overrides. Once you know what's in each layer, you can predict
  the result.
- **Host gets everything; containers get a subset.** A
  host-executed recipe inherits the whole merged set. A
  containerised recipe (`in = "<service>"` or under a
  devcontainer) only gets the **project-declared** keys forwarded
  via `-e KEY=VAL` — host `PATH`, `HOME`, etc. stay out so they
  don't override the container's own setup.
- **Per-key resolution has its own micro-rules.** Inside `[env]`,
  one key can declare several fallbacks (`value`, `base+offset`,
  `from_command`, `default`); first match wins. Pre-existing
  values from earlier layers count too.

## Common tasks

### Load a `.env` file

```toml
[env_files]
files = [".env", ".env.local"]
```

`${VAR}` expansion inside dotenv values resolves against earlier
layers, so a value in `.env.local` can reference a value from
`.env`.

### Set a default unless overridden

```toml
[env]
LOG_LEVEL = { default = "info" }
```

If `LOG_LEVEL` is already in the process env or a dotenv, that
wins. Otherwise `info`.

### Hard-set a value (trumps everything)

```toml
[env]
EDITOR = { value = "vim" }
```

Use `value` when you don't want the user's shell to be able to
override.

### Compute from a shell command

```toml
[env]
DATABASE_URL = { from_command = "scripts/db-url.sh", required = true }
GIT_SHA      = { from_command = "git rev-parse --short HEAD" }
```

The command runs once per ampelos invocation; stdout (trimmed) is
the value. `required = true` errors at preflight if no value
resolves.

### Vary a port by worktree

```toml
[env]
APP_PORT = { base = "8080", offset = "AMPELOS_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "AMPELOS_WORKTREE_OFFSET" }
```

`AMPELOS_WORKTREE_OFFSET` is injected automatically (see [Worktrees](Worktrees)).
Two checkouts get different offsets → different ports → no
collisions when both stacks are up.

### Per-recipe overrides

```toml
[command.test]
run = "composer test"
env = { XDEBUG_MODE = "off", APP_ENV = "testing" }
```

Recipe `env =` is the last layer. Activated profiles
(`--profile ci`) can add their own override layer on top.

### Make host shell vars visible inside a container

Container exec strips inherited process env on purpose. To
forward a host var explicitly, declare it under `[env]`:

```toml
[env]
GITHUB_TOKEN = { default = "" }   # default empty string; picks up the shell value if set
```

This becomes part of the project-declared subset that's forwarded
with `-e GITHUB_TOKEN=...`.

### Auto-write `.env` for tools outside ampelos

```toml
[worktrees]
dotenv = ".env"
```

Every `ampelos <anything>` invocation re-writes the managed block in
`.env` so `docker compose up` (run directly), IDE launch configs,
and `bin/rails s` all see the same values. The write is
idempotent — when the contents already match, the file's mtime
stays put.

`ampelos env --write .env` does the same one-shot.

### Inspect the resolved environment

```sh
ampelos env                # everything
ampelos env | grep '^APP_' # just APP_*
```

Useful when you've layered three `from_command`s and can't
remember which one won.

## Reference

### Layer order

In order, later wins:

1. **Inherited process env.** Whatever shell variables ampelos was
   launched with.
2. **Dotenv files**, in `[env_files].files` order. `${VAR}`
   expansion inside dotenv values resolves against earlier
   layers.
3. **`[env]` table**, key by key (see per-key resolution below).
4. **Recipe `env =` overrides** for the currently running recipe
   (and any active profile overlay).

### Per-key resolution under `[env]`

Each `[env.<KEY>]` is one of these shapes (fields combine inside
a single key declaration):

```toml
[env]
LOG_LEVEL  = { default = "info" }
APP_PORT   = { base = "8080", offset = "AMPELOS_WORKTREE_OFFSET" }
DB_URL     = { from_command = "scripts/db-url.sh", required = true }
EDITOR     = { value = "vim" }
```

Resolution within one key, first match wins:

1. `value` — hard-set; trumps everything else.
2. `base + offset` — integer-typed shorthand.
   `value = base.parse::<i64>() + existing[offset].parse::<i64>()`.
   Missing `offset` var falls back to `base`. Non-integer base errors.
3. Pre-existing process / dotenv value for the same name.
4. `from_command` stdout (trimmed).
5. `default`.

`required = true` with no resolved value errors at preflight.

### Host execution vs container exec

Host: child process inherits the full merged set above —
`PATH`, `HOME`, `cargo` and `git` find what they expect.

Container (`in = "<service>"` or under a devcontainer): only the
**project-declared** subset is forwarded via `-e KEY=VAL`:

- Keys loaded from `[env_files]` dotenv files.
- Keys defined under `[env]`.
- Injected `AMPELOS_WORKTREE_*` / `COMPOSE_PROJECT_NAME` vars.
- Per-recipe / per-script `env = {...}` overrides.

Inherited host process env is intentionally **not** propagated —
leaking the host `PATH` via `-e PATH=...` overrides the
container's own and breaks command resolution (`exec "sh": not
found`).

### Built-in worktree variables

Always injected, ahead of `[env]` resolution:

| Key | Source |
|---|---|
| `AMPELOS_WORKTREE_SLUG` | Detected from the active git checkout. |
| `AMPELOS_WORKTREE_OFFSET` | Pinned (`[worktrees.assign]`) or hashed from the slug. |
| `COMPOSE_PROJECT_NAME` | `<project>-<slug>` when `[worktrees].isolate_compose` is on and the user hasn't set it. |

### Script-only variables

Set by the script and install-flow runners, **outside** the merge
pipeline. Reach `.ampelos/commands/` scripts and `.ampelos/install/`
steps only — not `[command.*]` recipes, not the dotenv writer,
not `ampelos env`:

| Key | Source |
|---|---|
| `AMPELOS_PROJECT_DIR` | Host path to the worktree project root. |
| `AMPELOS_SCRIPT_DIR`  | Host path to the script file's parent directory. |

Both are host-side paths even when the script runs inside a
service or devcontainer. Inline install steps get
`AMPELOS_PROJECT_DIR` only — there's no script file for
`AMPELOS_SCRIPT_DIR` to point at.

See [Recipes and Scripts](Recipes-and-Scripts#environment-variables-provided-to-scripts)
and [Install Flow](Install-Flow#environment-variables) for the
script contracts in full.

## See also

- [Worktrees](Worktrees) for the slug + offset model that powers
  `base + offset`.
- [Hooks](Hooks) for how the post-checkout / post-merge auto-wiring
  keeps `.env` fresh.
- [Configuration Reference: `[env]`](Configuration-Reference#env-and-env_files).
