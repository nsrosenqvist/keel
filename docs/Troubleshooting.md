# Troubleshooting

Start with `ampelos doctor` — it validates config, checks backend
availability, probes service status commands, and reports per-issue
guidance. Most problems below have a doctor message that points you
straight at the cause.

```sh
ampelos doctor
```

## Configuration errors

### "unknown field `xxx` in `[command.foo]`"

ampelos uses `deny_unknown_fields` on every TOML section. Typo or a
removed field. Cross-check the
[Configuration Reference](Configuration-Reference).

### "duplicate service name `<name>`"

A name appears in two of:
[`[[services.custom]]`](Non-Container-Services), `[[services.systemd]]`,
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

Install Docker, or set `[runtime].backend = "none"` if you only
use [`[[services.custom]]`](Non-Container-Services) /
`[[services.systemd]]` declarations.

### Recipe with `in = "<svc>"` errors with "no such service"

The compose service isn't defined in `docker-compose.yml`. Check
`docker compose config --services` against the recipe.

### `ampelos <svc> php -v` doesn't exec inside the container

Either `[runtime].service_passthrough = false` is set, or the
name resolved to something earlier in the [resolution
order](Recipes-and-Scripts#resolution-order) (a recipe or
script with the same name). Use `ampelos which <svc>` to see how it
resolves.

## Environment issues

### `[env]` value not visible to `docker compose up`

Tools invoked outside ampelos don't see the resolved `[env]`. Set
[`[worktrees].dotenv = ".env"`](Worktrees#materialising-worktree-env-into-env)
to materialise the resolved env into `.env` automatically.

### "required env `<KEY>` could not be resolved"

`[env.<KEY>]` has `required = true` but no source produced a value.
Either drop `required`, set `default`, define `value`, or supply
the key via the process environment / dotenv.

### Two worktrees clash on ports

Use `base + offset` arithmetic with `AMPELOS_WORKTREE_OFFSET`:

```toml
[env]
APP_PORT = { base = "8080", offset = "AMPELOS_WORKTREE_OFFSET" }
```

`ampelos worktree list` shows every worktree's computed offset and
warns on collisions. Pin a slug explicitly with `ampelos worktree
assign <slug> <n>` if needed. See [Worktrees](Worktrees).

## Hook issues

### "refusing to overwrite non-ampelos hook at <path>"

`.git/hooks/<stage>` exists and isn't a ampelos-managed shim. Move
it aside (e.g. `.git/hooks/pre-commit.bak`) and rerun
`ampelos hooks install`. See [Hooks](Hooks).

### "hook `<id>` uses language `<lang>`; ampelos runs only `system` / `script` hooks"

ampelos runs only `language: system` and `language: script` hooks
natively. Wrap the tool with a shell script and use `language:
script`, or use a `repo: local` hook calling the tool already on
`PATH` with `language: system`.

### "`repo: meta` references pre-commit's built-in hooks"

ampelos doesn't implement the `meta` repo. Remove the entry or
replace it with an equivalent `repo: local` hook.

### `ampelos install --update-hooks` to fix a moved tag

When an upstream pre-commit repo moves a tag, the cached SHA goes
stale. `--update-hooks` clears the cache entry and re-clones at the
same rev. See [Hooks](Hooks#external-pre-commit-repos).

## Install flow issues

### "Previous install stopped at `<step>`. Resume from there?"

A previous `ampelos install` failed mid-flight and left state. Press
Enter (or `--resume` non-interactively) to pick up where it
stopped, or `--restart` to wipe state and start over. See
[Install Flow](Install-Flow#state-and-resume).

### Single new step everyone needs to apply

`ampelos install <step-name>` runs that step in isolation and updates
only its record. Recommended pattern when a maintainer adds one new
step that every teammate needs (e.g. `ampelos install
rebuild-search-index`).

## Agents issues

### "agents source `<src>` would write `<dest>` but a non-managed file already exists there"

A local file in a `[[dir]]` target has the same name as an upstream
file. Rename the local file (e.g. `foo.local.md`) so the conflict
is unambiguous. See [Agents](Agents#whole-file-ownership).

### Drift warning on `ampelos agents install`

You hand-edited a ampelos-owned file. By default ampelos leaves it
alone (warning only); `--force-overwrite-drift` overwrites it from
upstream. See [Agents](Agents#state--drift).

### Floating ref didn't refetch

`ampelos agents install` reuses cache for floating refs too — only
`ampelos agents update` (or `install --force`) re-fetches them. See
[Agents](Agents#floating-refs).

## TUI issues

### Service rows missing in the sidebar

Compose detection failed. Run `docker compose config --services`
manually. If output is fine but ampelos still doesn't show rows,
declare them explicitly via `[[ui.pane]] type = "service"` so they
appear regardless of detection. See [TUI](TUI#explicit-panes-uipane).

### `G` (diff view) shows wrong base branch

ampelos picks trunk via `origin/HEAD` → `main` → `master` →
`develop` → `trunk`. Override with `[diff] base = "release/stable"`
in `ampelos.toml`. See [Diff View](Diff-View#manual-override).

## Still stuck?

Open an issue at
[github.com/nsrosenqvist/ampelos/issues](https://github.com/nsrosenqvist/ampelos/issues)
with the output of `ampelos doctor` and the relevant section of
`ampelos.toml`.
