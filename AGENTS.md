# AGENTS.md

Instructions for AI agents (and humans) working on this repository.

## North Star

**scaffl is the dev-loop wrapper that adapts to your project, instead of forcing your project to adapt to it.**

Every developer ends up writing a `dev` shell script per project: it preflights containers, routes commands between host and container, wraps recurring tasks (`up`, `shell`, `test`, `migrate`, `check`), and forwards args to in-container tooling. The script grows, refactors, and never quite leaves the repo it was born in. Tools like DDEV solve this by enforcing a rigid format. Tools like `just` or `mprocs` solve a slice and stop.

scaffl is the union: a single binary that

1. **Defines commands two ways** — declaratively in `scaffl.toml` *or* as plain scripts under `.scaffl/commands/`. Use whichever shape matches the command's complexity.
2. **Knows where commands run** — host or service, via a Backend abstraction. Compose first; podman/docker pluggable.
3. **Doubles as a TUI dashboard** where you attach to service logs *and trigger commands* — not just a log viewer.
4. **Handles dev setup and git hooks**, with `.pre-commit-config.yaml` compatibility so projects can adopt scaffl without abandoning their existing hook ecosystem.

The user-visible promise: `scaffl init` in any Compose project produces a working dev loop in under a minute, and replacing your hand-rolled `dev` script with a `scaffl.toml` is a strict win, not a sideways move.

The architectural promise: each capability lives in its own crate with a focused trait surface. The CLI and the TUI are different views of the same runtime, never two implementations of the same logic.

## Operating principles

These shape every code change in the repo:

- **SOLID.** Single-responsibility crates; backends and runners depend on traits, not concretes; small, focused interfaces.
- **DDD.** Bounded contexts split as crates: `config`, `runtime`, `container`, `hooks`, `tui`. Cross-context types travel through well-defined value objects, not shared mutable state.
- **Performance is a default, not an afterthought.** Stream output, don't buffer it. Prefer `&str` and `Cow<'_, str>` over owning `String` where lifetimes allow. Avoid clone-happy code. No runtime reflection — TOML schemas are serde-derived at compile time.
- **One source of truth per concern.** A recipe is defined once. The CLI runs it. The TUI runs it. Both go through `scaffl-runtime`.
- **No dead config.** Every option in `scaffl.toml` must change observable behaviour, or it doesn't ship.

## Required verification step

After **every** code change, run the full verification ladder before committing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If any step fails, fix the cause — don't suppress warnings, don't `#[allow]` clippy lints without a written justification in the commit body, and don't `--no-verify` past pre-commit hooks.

For UI / TUI changes specifically: also run a manual smoke test (`cargo run -- ui` against the example project under `examples/`) before reporting the change as complete.

## Commits — Conventional Commits

All commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<optional body explaining the why>

<optional footers, e.g. BREAKING CHANGE:>
```

**Types** used in this repo: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `build`, `ci`, `perf`.

**Scopes** match crate or area: `config`, `runtime`, `container`, `tui`, `hooks`, `cli`, `workspace`, `docs`.

**Subject**: imperative, lowercase, no trailing period. Under 70 characters.

**Body**: explain *why*, not *what*. The diff already shows the what.

Examples:

- `feat(config): support env var expansion in run strings`
- `fix(runtime): propagate non-zero exit codes from compose exec`
- `refactor(container): extract Backend trait from compose impl`
- `perf(runtime): stream stdout instead of buffering before printing`
- `docs(agents): clarify when to use scripts vs recipes`

Breaking changes: add `BREAKING CHANGE: <description>` as a footer, and add `!` after type/scope: `feat(config)!: rename run_args to forward_args`.

## Worktree-aware environments

scaffl detects the current git checkout and gives each one a
deterministic identity (slug + integer offset). Recipes use the offset
to vary ports and the like, so two worktrees of the same project can
run side-by-side without collisions.

### Loading order

```
1. Built-in defaults
2. scaffl.toml
3. .scaffl/local.toml                        (per-developer overrides)
4. .scaffl/worktrees/<slug>.toml             (per-worktree overrides)
5. SCAFFL_WORKTREE_SLUG / _OFFSET injected
6. COMPOSE_PROJECT_NAME injected (if isolate_compose, slug non-empty,
   user hasn't already set it)
7. [env] resolution (can reference SCAFFL_WORKTREE_OFFSET)
```

Per-worktree overlays live in the *current working directory's*
`.scaffl/worktrees/<slug>.toml`. Each git worktree has its own working
tree, so each maintains its own overlay file (or share via symlink).

### Identity

`slug` derives from (in order): branch name → linked-worktree
directory basename → `det-<7-char SHA>` for detached HEAD →
empty string for non-git directories. Slugification is
`[^a-z0-9-]` → `-`, runs collapsed, trimmed.

`offset` is `[worktrees.assign][slug]` if pinned, otherwise
`fnv1a_32("<seed>|<slug>") % modulus`. Default modulus is 1000;
seed defaults to `[project] name`. Empty slug → offset 0.

### `[env]` arithmetic

```toml
[env]
APP_PORT = { base = "8080", offset = "SCAFFL_WORKTREE_OFFSET" }
```

Resolves to `base.parse::<i64>() + existing[offset].parse::<i64>()`.
Missing offset var → falls back to `base`. Non-integer base → error.
Use this instead of shell math in `from_command`.

### CLI

- `scaffl worktree status` — current slug, offset, isolation, derived
  env values.
- `scaffl worktree list` — every git worktree's computed offset, with
  collision warnings.
- `scaffl worktree assign <slug> <n> [--local]` — pin a slug. Without
  `--local`, writes to `scaffl.toml` (team-wide; commit to share).
  With `--local`, writes to `.scaffl/local.toml` (per-developer, this
  checkout only).

### Materialising worktree env into `.env`

The `[env]` arithmetic is only visible inside scaffl's process tree.
Tools invoked outside scaffl (`docker compose up` directly, IDE-launched
servers, `bin/rails s`, `npm run dev`, …) read `.env` and don't see the
worktree-derived values.

The simplest fix is one config line:

```toml
[worktrees]
dotenv = ".env"
```

When set, two things happen:

1. **Auto-write on every scaffl invocation.** The resolved
   `[env]` plus the three worktree-derived built-ins
   (`SCAFFL_WORKTREE_SLUG`, `_OFFSET`, `COMPOSE_PROJECT_NAME`)
   land in the file as a marker-delimited block. The write is
   idempotent — when the contents already match, the file isn't
   touched (mtime stays put), so file watchers and `git status`
   don't see spurious churn.
2. **`scaffl hooks install` auto-includes `post-checkout` and
   `post-merge`.** That keeps the file fresh after a branch switch
   even when the developer goes on to run `docker compose up`
   directly without involving scaffl.

User content above and below the managed block is preserved;
the block itself is replaced in place on each write. Path is
project-root-relative unless absolute.

For the explicit / one-shot form, `scaffl env --write [PATH]` writes
the same block ad-hoc — useful in CI scripts or when you don't want
the every-invocation auto-write.

## Non-container services

Some projects mix compose-managed containers with services
controlled some other way (a system Postgres, a brew-installed
Redis, a tunnel daemon). scaffl handles them through two config
shapes that compile down to the same internal `CustomBackend`:

```toml
# Generic — you describe how to control it.
[[services.custom]]
name   = "ngrok"
status = "pgrep -x ngrok"          # exit 0 = Running; non-zero = Stopped
start  = "ngrok http 8080 > /tmp/ngrok.log 2>&1 &"
stop   = "pkill -x ngrok"
restart = "..."                    # optional; defaults to stop && start
logs    = "tail -f /tmp/ngrok.log" # optional; absent → no log tailing
desc    = "Public tunnel"          # optional; shown in the TUI

# Sugar — fills in start/stop/restart/status/logs from systemctl.
[[services.systemd]]
name  = "postgres"
unit  = "postgresql.service"
scope = "user"                     # "user" (default) | "system"
```

Service names must be unique across `services.custom`,
`services.systemd`, and the compose project (collisions error
at config-load time). The TUI sidebar tags each row with its
backend kind; the keymap (`r` / `R` / `s` / `S` / `U` / `D`)
operates uniformly across all kinds.

`[containers] backend = "none"` plus only `[[services.systemd]]` /
`[[services.custom]]` declarations is fully supported — useful
when the project's stack is entirely host-managed.

`scaffl doctor` probes each declared service's status command and
reports running / stopped per entry, so you can verify the
declarations are correct before opening the TUI.

## TUI service panes

Two paths populate the service rows in the TUI sidebar:

1. **Auto-discovery** — at startup, scaffl asks the active backend
   for its service list (`docker compose config --services` for
   compose). Every name shows up as a service row in alphabetical
   order. Most projects need nothing else.

2. **Explicit `[[ui.pane]] type = "service"`** — only worth adding
   when you want one of:
   - A pinned ordering (declared services come first in scaffl.toml
     order; auto-discovered services follow alphabetically).
   - A `key = "1"` shortcut for direct focus.
   - A row that exists *before* compose detection runs (useful on
     slow setups or when compose detection might fail and you still
     want the row visible).

Without one of those reasons, omit the block — auto-discovery does
the same job and the redundant declarations rot when the
docker-compose.yml service list changes.

## Diff view

The diff view (`G`) is a built-in branch-review surface anchored to
the merge-base with the project's trunk branch. Scope: every file
that differs from the merge-base — committed-since-branching plus
working-tree changes plus untracked files (filtered through
`.gitignore`). Not the working-tree-vs-last-commit slice that `git
diff HEAD` shows.

Trunk resolution order:

1. `[diff] base = "..."` in `scaffl.toml` if set.
2. `git symbolic-ref refs/remotes/origin/HEAD` — the remote default
   branch when a remote is configured.
3. Local fallback: `main`, `master`, `develop`, `trunk`, in order.
4. None of the above → fall back to `git diff HEAD` so the view
   still works in repos with no trunk yet (fresh `git init`,
   detached repos, etc.).

The chosen trunk is surfaced in the top bar as `<branch> vs <trunk>`
so users always see what the file count and per-file diffs are
anchored against. The merge-base SHA is recomputed on every
refresh (`r` in the diff view, or any view-switch back to it), so
`git pull origin main` advancing the trunk shifts subsequent
comparisons forward instead of staying pinned.

Override exists for projects that don't follow the conventional
trunk names — `release/stable`, `dev`, etc. Set `[diff] base =
"release/stable"` and detection short-circuits before any git
lookup.

## Install flow

`scaffl install` is the project-bootstrap surface: a teammate clones
the repo, runs one command, and gets a working dev environment.

### Step plan

The plan is resolved from two sources, in this precedence:

1. `[install].steps = [...]` in `scaffl.toml`. Each entry is either
   a name (resolves to a `[command.*]` recipe or to a discovered
   step file) or an inline `{ name, run, in, env, cwd, optional,
   interactive }` table.
2. Files discovered under `.scaffl/install/`. Same `# @key:`
   frontmatter parser as `.scaffl/commands/`, plus install-only
   keys `cwd`, `optional`, `interactive`. When `[install].steps` is
   empty, discovered files **are** the plan (file-name sorted).

A synthetic `install-hooks` step is appended automatically unless
`[install] install_git_hooks = false`. It installs git hook shims
and prefetches any external `.pre-commit-config.yaml` repos into
`.scaffl/cache/hooks/<rev>/`.

Install steps are deliberately separate from `Config.scripts` /
`Config.commands` — they do not appear in `scaffl list` or the TUI
sidebar.

### State and resume

Every successful or failed step is recorded in
`.scaffl/install.state.json`. On a subsequent `scaffl install`:

- If every step is `ok` or `skipped`, the plan re-runs from step 1.
- Otherwise the user is prompted "Resume from `<step>`?". `--resume`
  bypasses the prompt; `--restart` wipes state and starts fresh.

`scaffl install <step>` runs one step in isolation and updates only
that step's record. Useful when a maintainer adds a new step that
every teammate needs to apply ("everyone please run `scaffl install
rebuild-search-index` once").

### Renderer

A small crossterm line-redraw printer in
`crates/scaffl-cli/src/commands/install/renderer.rs` — **not** a
TUI. Each step gets a row that updates in place (◐ running with
spinner → ✓ ok / ✗ failed / → skipped, plus duration). The active
step's tail output (last 3 lines) shows below its row.

Interactive steps (`# @interactive: yes`) pause the renderer and
inherit the parent's stdio for the duration of the step. That's how
`scaffl lib ask | confirm | password | select | filter` work inside
install steps without an IPC sentinel protocol — the step really
does have the terminal.

### Hook auto-fetch (no `pre-commit` dependency)

External repos in `.pre-commit-config.yaml` are cloned into
`.scaffl/cache/hooks/<slug(url)-rev>/` by `scaffl-hooks/src/cache.rs`.
The runner reads `.pre-commit-hooks.yaml` inside the clone to find
each hook's `entry` and `language`, merges with the user's
`HookSpec`, and runs it natively. **No** fallback to the `pre-commit`
binary; that path is deleted. Unsupported `language:` values
(`python`, `node`, `ruby`, …) and `repo: meta` error at install time
with a clear message instead of silently skipping.

### `.scaffl/.gitignore`

`scaffl install` writes a marker-delimited managed block in
`.scaffl/.gitignore` covering `local.toml`, `worktrees/`, `cache/`,
and `install.state.json`. Idempotent — re-running install when the
file is already correct leaves the mtime alone. Path is
configurable via `[install] gitignore = "..."`. The shared
`crates/scaffl-config/src/managed_block.rs` helper is the single
implementation of the marker pattern (also used by the worktree
dotenv writer).

## Layout

```
crates/
  scaffl-cli/        # binary; clap; subcommand dispatch
  scaffl-config/     # TOML / YAML parsing; schema; env resolution
  scaffl-runtime/    # recipe resolution; supervision; preflight
  scaffl-container/  # Backend trait; compose / docker / podman impls
  scaffl-tui/        # ratatui app; panes; palette
  scaffl-hooks/      # .pre-commit-config.yaml reader; git hook installer; cache
examples/            # fixture projects used by integration tests
```

The full design plan lives in the original plan file referenced in the project README; this document supersedes any contradiction.

## When in doubt

- Default to fewer features, smaller surface area, sharper traits.
- A single integration test that runs against a real fixture beats five mocked unit tests.
- If a change makes `scaffl` slower for the common case, it does not ship without a measurement.
- Read `scaffl.toml` semantics conservatively: silent inference is a bug.
