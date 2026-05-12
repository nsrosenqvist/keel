# Environments

keel resolves the environment a command sees from four sources,
deep-merged in a documented order. The same model drives recipe
execution, compose preflight, and the
`[worktrees].dotenv` writer.

## Layers

In order, later wins:

1. **Inherited process env.** Whatever shell variables keel was
   launched with.
2. **Dotenv files**, in `[env_files].files` order:

   ```toml
   [env_files]
   files = [".env", ".env.local"]
   ```

   `${VAR}` expansion inside dotenv values resolves against earlier
   layers.
3. **`[env]` table**, key by key.
4. **Recipe `env =` overrides** for the recipe currently running.

## Host execution vs. container exec

When a recipe runs on the host (no `in = "<service>"`, no devcontainer),
the child process inherits the full merged set above — process env,
dotenv files, `[env]`, recipe overrides — so commands like `git` and
`cargo` find their PATH the way they normally would.

When a recipe runs inside a container — either `in = "<compose-service>"`
or under an opt-in devcontainer — only the **project-declared** subset
is forwarded via `-e KEY=VAL`:

- Keys loaded from `[env_files]` dotenv files.
- Keys defined under `[env]`.
- The injected `KEEL_WORKTREE_*` / `COMPOSE_PROJECT_NAME` vars.
- Per-recipe / per-script `env = {...}` overrides.

Inherited process env (host `PATH`, `HOME`, `USER`, `SHELL`, …) is
intentionally **not** propagated. The container's image defaults and
the devcontainer spec's own `containerEnv` / `remoteEnv` provide those
— leaking the host values via `-e PATH=...` overrides the container's
PATH and breaks command resolution (`exec "sh": not found`). To make
a host-shell var visible inside a container, declare it explicitly in
`[env]` (e.g. `MY_TOKEN = { default = "" }` to pick up the existing
process value, or `from_command = "..."` to derive it).

## `[env]` per-key resolution

Each `[env.<KEY>]` is one of these shapes (spec fields combine):

```toml
[env]
LOG_LEVEL  = { default = "info" }
APP_PORT   = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
DB_URL     = { from_command = "scripts/db-url.sh", required = true }
EDITOR     = { value = "vim" }
```

Resolution order **within one key**, first match wins:

1. `value` — hard-set; trumps everything else.
2. `base + offset` — integer-typed shorthand.
   `value = base.parse::<i64>() + existing[offset].parse::<i64>()`.
   Missing `offset` var falls back to `base`. Non-integer base errors.
3. Pre-existing process / dotenv value for the same name.
4. `from_command` stdout (trimmed).
5. `default`.

`required = true` with no resolved value errors at preflight time.

## Built-in worktree variables

Three keys are always injected into every recipe and dotenv writer:

| Key | Source |
|---|---|
| `KEEL_WORKTREE_SLUG` | Detected from git checkout. |
| `KEEL_WORKTREE_OFFSET` | Pinned (`[worktrees.assign]`) or hashed. |
| `COMPOSE_PROJECT_NAME` | `<project>-<slug>` if `[worktrees].isolate_compose`. |

`base + offset` is the typical consumer:

```toml
[env]
APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "KEEL_WORKTREE_OFFSET" }
```

Two checkouts of the same project get different ports automatically.
See [Worktrees](Worktrees) for the slug-and-offset model in
full.

## Inspecting the resolved environment

`keel env` prints every resolved key as `KEY=VALUE` lines. Use it
to sanity-check the merge:

```sh
keel env | grep '^APP_'
APP_PORT=8083
APP_ENV=local
```

`keel env --write .env` writes the same content into a managed
block in `.env` — the file's user-owned content above and below the
markers is preserved.

## Materialising into `.env` automatically

Tools invoked outside keel (`docker compose up` directly, IDE
launch configs, `bin/rails s`, `npm run dev`, …) read `.env` and
won't see keel-only values. One config line bridges the gap:

```toml
[worktrees]
dotenv = ".env"
```

When set:

1. Every `keel <anything>` invocation re-writes the managed block
   in `.env`. Idempotent — when the contents already match, the
   file's mtime stays put, so file watchers and `git status` don't
   churn.
2. `keel hooks install` (without an explicit `--stages` list)
   auto-includes `post-checkout` and `post-merge` so the file stays
   fresh after a branch switch even when the developer skips keel.

For a one-shot write outside keel's normal lifecycle, use
`keel env --write .env` directly — useful in CI scripts.

## See also

- [Configuration Reference: `[env]`](Configuration-Reference#env-and-env_files)
- [Worktrees](Worktrees) for the slug + offset model.
- [Hooks](Hooks) for how the post-checkout / post-merge auto-wiring works.
