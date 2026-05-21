# Worktrees

If you use `git worktree add` to keep more than one branch of the
same project checked out at once, you've probably hit the
port-collision problem: both stacks try to bind `8080`, both
docker-compose projects share the same containers, both produce
the same `COMPOSE_PROJECT_NAME`. `croft` solves this by detecting
the current checkout and giving each one a deterministic
**offset** you can subtract from port numbers and use to scope
container names.

## Quickstart

**1. Make a port worktree-aware:**

```toml
[env]
APP_PORT = { base = "8080", offset = "CROFT_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "CROFT_WORKTREE_OFFSET" }
```

**2. Check it in two worktrees:**

```sh
# main checkout
$ croft env | grep APP_PORT
APP_PORT=8080

# in a parallel `git worktree add ../proj-feat feature/login`
$ croft env | grep APP_PORT
APP_PORT=8087
```

The number after `8080` is the worktree's offset. Different
branch → different slug → different offset → different port.

**3. (Optional) Switch between them in the TUI:**

```sh
croft        # opens the dashboard
W           # opens the worktree switcher modal
```

## Mental model

- **Each worktree has a slug.** Derived from the branch name (or
  the worktree directory basename, or a short SHA for detached
  HEAD). It's how croft identifies "which checkout am I in."
- **Each slug has an offset.** Either pinned in
  `[worktrees.assign]` or hashed from the slug + a per-project
  seed. Default modulus is 1000, so offsets live in 0..999.
- **You consume the offset.** Read `CROFT_WORKTREE_OFFSET`
  directly, or — more often — use the `base + offset` shape in
  `[env]` so derived values fall out automatically.
- **Compose stacks isolate too.** When
  `[worktrees].isolate_compose = true` (the default),
  `COMPOSE_PROJECT_NAME` is set to `<project>-<slug>` so each
  worktree gets its own containers, networks, and volumes.

## Common tasks

### Compute a per-worktree port

```toml
[env]
APP_PORT = { base = "8080", offset = "CROFT_WORKTREE_OFFSET" }
```

`base.parse::<i64>() + CROFT_WORKTREE_OFFSET.parse::<i64>()`. Pair
it with a compose file that reads `${APP_PORT}` and the host
binding is unique per checkout.

### Pin a specific offset to a specific branch

If you want `main` to always be offset `0` and `production` also
`0` (because you never run them together), but `feature/x` to be
`7`:

```sh
croft worktree assign main 0
croft worktree assign production 0
croft worktree assign feature/x 7
```

Or directly in `croft.toml`:

```toml
[worktrees.assign]
main         = 0
production   = 0
"feature/x"  = 7
```

`croft worktree assign --local` writes to `.croft/local.toml`
instead — useful for personal overrides you don't want to share
with the team.

### Switch between worktrees inside the TUI

`W` in the dashboard opens a modal:

- Lists every git worktree under the repo.
- Hot-reloads croft into the chosen one without restarting.
- The "+ new worktree" entry opens a branch-first picker — type
  to filter local + remote branches, pick one to attach an
  existing checkout, or take the **"create branch '<input>' off
  HEAD"** sentinel for `git worktree add -b`.

The path field auto-fills as `<parent>/<slug>` but Tab into it
for a manual override.

### See what's resolved

```sh
croft worktree status    # current slug, offset, isolation, env values
croft worktree list      # every worktree's offset, with collision warnings
```

`list` flags two slugs that hash to the same offset — when that
happens, pin one of them with `assign`.

### Share env with tools outside croft

`CROFT_WORKTREE_OFFSET` and the `base + offset` result are only
visible inside croft's process tree. To make them available to
`docker compose up` (run directly), IDE launchers, etc.:

```toml
[worktrees]
dotenv = ".env"
```

Every `croft <anything>` invocation rewrites the managed block in
`.env`. Idempotent: when the content already matches, the file's
mtime stays put, so file watchers and `git status` don't churn.

`croft hooks install` (without an explicit `--stages` list) also
auto-adds `post-checkout` and `post-merge` shims so `.env` stays
fresh after a branch switch even when the developer skips croft.

### Per-worktree config overrides

Drop overrides into `.croft/worktrees/<slug>.toml` to apply only
in that checkout:

```toml
# .croft/worktrees/feature-x.toml
[env]
FEATURE_FLAG_NEW_UI = { value = "1" }
```

These layer between `croft.toml` and the injected
`CROFT_WORKTREE_*` vars. See [loading order](#loading-order) below.

## Reference

### Slug derivation

In order, first match wins:

1. The current branch name.
2. The linked-worktree directory basename (`git worktree add`'s
   target dir).
3. `det-<7-char SHA>` for detached HEAD.
4. Empty string for non-git directories.

Slugification: `[^a-z0-9-]` → `-`, runs collapsed, leading /
trailing dashes trimmed.

### Offset assignment

`[worktrees.assign][slug]` if pinned, otherwise
`fnv1a_32("<seed>|<slug>") % modulus`. Default modulus is `1000`;
seed defaults to `[project].name`. Empty slug → offset `0`.

### Loading order

Per-worktree config layering, later wins:

```
1. Built-in defaults
2. croft.toml
3. .croft/local.toml                   (per-developer overrides)
4. .croft/worktrees/<slug>.toml        (per-worktree overrides)
5. CROFT_WORKTREE_SLUG / _OFFSET injected
6. COMPOSE_PROJECT_NAME injected (if isolate_compose, slug non-empty,
   user hasn't already set it)
7. [env] resolution (can reference CROFT_WORKTREE_OFFSET)
```

Per-worktree overlays live in the *current working directory's*
`.croft/worktrees/<slug>.toml`. Each git worktree has its own
working tree, so each maintains its own overlay file (or share
via symlink).

### Compose isolation

`[worktrees].isolate_compose = true` (the default) sets
`COMPOSE_PROJECT_NAME = <project>-<slug>` so each worktree's
docker-compose stack is independent — separate containers,
networks, and volumes. Skipped when the user has already set
`COMPOSE_PROJECT_NAME` themselves, so an explicit override always
wins.

### Built-in env variables (injected ahead of `[env]`)

| Key | Source |
|---|---|
| `CROFT_WORKTREE_SLUG` | Slug derivation, above. |
| `CROFT_WORKTREE_OFFSET` | Pinned or hashed. |
| `COMPOSE_PROJECT_NAME` | `<project>-<slug>` when `isolate_compose`. |

## Command reference

| Command | Notes |
|---|---|
| `croft worktree status` | Current slug, offset, isolation flag, derived env values. |
| `croft worktree list` | Every git worktree's computed offset, with collision warnings. |
| `croft worktree assign <slug> <n> [--local]` | Pin a slug to an offset. |

`croft worktree assign` without `--local` writes to `croft.toml`
(team-wide; commit it to share). With `--local`, writes to
`.croft/local.toml` (per-developer, this checkout only).

## See also

- [Environments](04-Environments) — `base + offset` lives there;
  this page covers the offset source.
- [Hooks](09-Hooks) — `post-checkout` / `post-merge` auto-wiring for
  the dotenv writer.
- [TUI](12-TUI) — the `W` worktree switcher modal.
- [Configuration Reference: `[worktrees]`](Configuration-Reference#worktrees).
