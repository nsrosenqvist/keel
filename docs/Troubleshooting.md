# Troubleshooting

Start with `keel doctor` — it validates config, checks backend
availability, probes service status commands, and reports per-issue
guidance. Most problems below have a doctor message that points you
straight at the cause.

```sh
keel doctor
```

## Configuration errors

### "unknown field `xxx` in `[command.foo]`"

keel uses `deny_unknown_fields` on every TOML section. Typo or a
removed field. Cross-check the
[Configuration Reference](./Configuration-Reference.md).

### "duplicate service name `<name>`"

A name appears in two of:
[`[[services.custom]]`](./Non-Container-Services.md), `[[services.systemd]]`,
or the compose project. Rename one. Errors at config-load time, not
at runtime.

### "duplicate agents source name `<name>`"

`[[agents.sources]]` `name` field must be unique. Rename one.

### "duplicate step name `<name>`"

The install plan has two steps with the same name. Resume can't
disambiguate, so the loader rejects it. Rename one (or remove the
duplicate from `[install].steps`).

## Container backend issues

### "compose backend selected but `docker` is not on PATH"

Install Docker, or set `[containers].backend = "none"` if you only
use [`[[services.custom]]`](./Non-Container-Services.md) /
`[[services.systemd]]` declarations.

### Recipe with `in = "<svc>"` errors with "no such service"

The compose service isn't defined in `docker-compose.yml`. Check
`docker compose config --services` against the recipe.

### `keel <svc> php -v` doesn't exec inside the container

Either `[containers].service_passthrough = false` is set, or the
name resolved to something earlier in the [resolution
order](./Recipes-and-Scripts.md#resolution-order) (a recipe or
script with the same name). Use `keel which <svc>` to see how it
resolves.

## Environment issues

### `[env]` value not visible to `docker compose up`

Tools invoked outside keel don't see the resolved `[env]`. Set
[`[worktrees].dotenv = ".env"`](./Worktrees.md#materialising-worktree-env-into-env)
to materialise the resolved env into `.env` automatically.

### "required env `<KEY>` could not be resolved"

`[env.<KEY>]` has `required = true` but no source produced a value.
Either drop `required`, set `default`, define `value`, or supply
the key via the process environment / dotenv.

### Two worktrees clash on ports

Use `base + offset` arithmetic with `KEEL_WORKTREE_OFFSET`:

```toml
[env]
APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
```

`keel worktree list` shows every worktree's computed offset and
warns on collisions. Pin a slug explicitly with `keel worktree
assign <slug> <n>` if needed. See [Worktrees](./Worktrees.md).

## Hook issues

### "refusing to overwrite non-keel hook at <path>"

`.git/hooks/<stage>` exists and isn't a keel-managed shim. Move
it aside (e.g. `.git/hooks/pre-commit.bak`) and rerun
`keel hooks install`. See [Hooks](./Hooks.md).

### "hook `<id>` uses language `<lang>`; keel runs only `system` / `script` hooks"

keel runs only `language: system` and `language: script` hooks
natively. Wrap the tool with a shell script and use `language:
script`, or use a `repo: local` hook calling the tool already on
`PATH` with `language: system`.

### "`repo: meta` references pre-commit's built-in hooks"

keel doesn't implement the `meta` repo. Remove the entry or
replace it with an equivalent `repo: local` hook.

### `keel install --update-hooks` to fix a moved tag

When an upstream pre-commit repo moves a tag, the cached SHA goes
stale. `--update-hooks` clears the cache entry and re-clones at the
same rev. See [Hooks](./Hooks.md#external-pre-commit-repos).

## Install flow issues

### "Previous install stopped at `<step>`. Resume from there?"

A previous `keel install` failed mid-flight and left state. Press
Enter (or `--resume` non-interactively) to pick up where it
stopped, or `--restart` to wipe state and start over. See
[Install Flow](./Install-Flow.md#state-and-resume).

### Single new step everyone needs to apply

`keel install <step-name>` runs that step in isolation and updates
only its record. Recommended pattern when a maintainer adds one new
step that every teammate needs (e.g. `keel install
rebuild-search-index`).

## Agents issues

### "agents source `<src>` would write `<dest>` but a non-managed file already exists there"

A local file in a `[[dir]]` target has the same name as an upstream
file. Rename the local file (e.g. `foo.local.md`) so the conflict
is unambiguous. See [Agents](./Agents.md#whole-file-ownership).

### Drift warning on `keel agents install`

You hand-edited a keel-owned file. By default keel leaves it
alone (warning only); `--force-overwrite-drift` overwrites it from
upstream. See [Agents](./Agents.md#state--drift).

### Floating ref didn't refetch

`keel agents install` reuses cache for floating refs too — only
`keel agents update` (or `install --force`) re-fetches them. See
[Agents](./Agents.md#floating-refs).

## TUI issues

### Service rows missing in the sidebar

Compose detection failed. Run `docker compose config --services`
manually. If output is fine but keel still doesn't show rows,
declare them explicitly via `[[ui.pane]] type = "service"` so they
appear regardless of detection. See [TUI](./TUI.md#explicit-panes-uipane).

### `G` (diff view) shows wrong base branch

keel picks trunk via `origin/HEAD` → `main` → `master` →
`develop` → `trunk`. Override with `[diff] base = "release/stable"`
in `keel.toml`. See [Diff View](./Diff-View.md#manual-override).

## Still stuck?

Open an issue at
[github.com/nsrosenqvist/keel/issues](https://github.com/nsrosenqvist/keel/issues)
with the output of `keel doctor` and the relevant section of
`keel.toml`.
