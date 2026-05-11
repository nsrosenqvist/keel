# Worktrees

keel detects the current git checkout and gives each one a
deterministic identity (slug + integer offset). Recipes use the
offset to vary ports and other per-checkout values, so two worktrees
of the same project can run side-by-side without collisions.

## Identity

The slug derives from, in order:

1. The current branch name.
2. The linked-worktree directory basename (`git worktree add`'s
   target dir).
3. `det-<7-char SHA>` for detached HEAD.
4. Empty string for non-git directories.

Slugification: `[^a-z0-9-]` → `-`, runs collapsed, leading/trailing
dashes trimmed.

The offset is `[worktrees.assign][slug]` if pinned, otherwise
`fnv1a_32("<seed>|<slug>") % modulus`. Default modulus is 1000; seed
defaults to `[project].name`. Empty slug → offset 0.

## `[env]` arithmetic

```toml
[env]
APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
DB_PORT  = { base = "5432", offset = "KEEL_WORKTREE_OFFSET" }
```

Resolves to `base.parse::<i64>() + existing[offset].parse::<i64>()`.
Missing offset var → falls back to `base`. Non-integer base → error.
Use this instead of shell math in `from_command`.

See [Environments](./Environments.md) for the rest of the env
resolution model.

## Loading order

Per-worktree config layering, later wins:

```
1. Built-in defaults
2. keel.toml
3. .keel/local.toml                  (per-developer overrides)
4. .keel/worktrees/<slug>.toml       (per-worktree overrides)
5. KEEL_WORKTREE_SLUG / _OFFSET injected
6. COMPOSE_PROJECT_NAME injected (if isolate_compose, slug non-empty,
   user hasn't already set it)
7. [env] resolution (can reference KEEL_WORKTREE_OFFSET)
```

Per-worktree overlays live in the *current working directory's*
`.keel/worktrees/<slug>.toml`. Each git worktree has its own
working tree, so each maintains its own overlay file (or share via
symlink).

## CLI

| Command | Notes |
|---|---|
| `keel worktree status` | Current slug, offset, isolation flag, derived env values. |
| `keel worktree list` | Every git worktree's computed offset, with collision warnings. |
| `keel worktree assign <slug> <n> [--local]` | Pin a slug to an offset. |

`keel worktree assign` without `--local` writes to `keel.toml`
(team-wide; commit it to share). With `--local`, writes to
`.keel/local.toml` (per-developer, this checkout only).

```toml
[worktrees.assign]
main       = 0
production = 0
"feature/x" = 7
```

## Compose isolation

`[worktrees].isolate_compose = true` (the default) sets
`COMPOSE_PROJECT_NAME = <project>-<slug>` so each worktree's docker
compose stack is independent — separate containers, networks, and
volumes per checkout. Skipped when the user has already set
`COMPOSE_PROJECT_NAME` themselves, so an explicit override always
wins.

## Materialising worktree env into `.env`

The `[env]` arithmetic is only visible inside keel's process tree.
Tools invoked outside keel (`docker compose up` directly,
IDE-launched servers, `bin/rails s`, `npm run dev`, …) read `.env`
and don't see the worktree-derived values.

The simplest fix is one config line:

```toml
[worktrees]
dotenv = ".env"
```

When set, two things happen:

1. **Auto-write on every keel invocation.** The resolved `[env]`
   plus the three worktree-derived built-ins
   (`KEEL_WORKTREE_SLUG`, `_OFFSET`, `COMPOSE_PROJECT_NAME`) land
   in the file as a marker-delimited block. The write is idempotent
   — when the contents already match, the file isn't touched (mtime
   stays put), so file watchers and `git status` don't see spurious
   churn.
2. **`keel hooks install` auto-includes `post-checkout` and
   `post-merge`.** That keeps the file fresh after a branch switch
   even when the developer goes on to run `docker compose up`
   directly without involving keel.

User content above and below the managed block is preserved; the
block itself is replaced in place on each write. Path is
project-root-relative unless absolute.

For the explicit / one-shot form, `keel env --write [PATH]` writes
the same block ad-hoc — useful in CI scripts or when you don't want
the every-invocation auto-write.

## TUI: worktree switcher (`W`)

The TUI's `W` keymap opens a worktree-switcher modal: list every
checkout under the repo, hot-reload keel into a different worktree
without restarting. The "+ new worktree" entry opens a branch-first
picker — type to filter local + remote branches, pick one to attach,
or take the "create branch '<input>' off HEAD" sentinel for `git
worktree add -b`. The path field auto-fills as `<parent>/<slug>` but
Tab into it for a manual override.

## See also

- [Environments](./Environments.md) for env resolution details.
- [Hooks](./Hooks.md) for the post-checkout / post-merge wiring.
- [TUI](./TUI.md) for the worktree switcher modal.
- [Configuration Reference: `[worktrees]`](./Configuration-Reference.md#worktrees).
