# Configuration Reference

Every key in `keel.toml`, with type, default, and a one-line
example. The schema lives in
[`crates/keel-config/src/`](https://github.com/nsrosenqvist/keel/tree/main/crates/keel-config/src);
unknown keys are rejected at load time so typos don't silently
become no-ops.

keel loads config in three layers, deep-merged in order (later wins):

1. `keel.toml` at the project root.
2. `.keel/local.toml` (per-developer overrides; gitignored).
3. `.keel/worktrees/<slug>.toml` (per-worktree overrides; gitignored).

## `[project]`

| Key | Type | Default | Notes |
|---|---|---|---|
| `name` | string | unset | Used as the worktree-hash seed when `[worktrees].seed` isn't set. |
| `description` | string | unset | Surfaced by `keel doctor` and the TUI title. |

```toml
[project]
name        = "myapp"
description = "Public-facing API"
```

## `[containers]`

Container-runtime selection. See [Container Backends](./Container-Backends.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `backend` | `compose` / `docker` / `podman` / `none` | `compose` | `none` skips container preflight entirely. |
| `default_service` | string | unset | Default service for `in = "..."` recipes. |
| `compose_passthrough` | bool | `true` | Unmatched names fall through to `docker compose <cmd>`. |
| `service_passthrough` | bool | `true` | Unmatched names that match a compose service exec into it. |

## `[env]` and `[env_files]`

Per-key spec. See [Environments](./Environments.md).

```toml
[env_files]
files = [".env", ".env.local"]

[env]
APP_PORT = { base = "8080", offset = "KEEL_WORKTREE_OFFSET" }
DB_URL   = { from_command = "scripts/db-url.sh", required = true }
LOG      = { default = "info" }
```

| `[env.<KEY>]` | Type | Notes |
|---|---|---|
| `value` | string | Hard-set value; wins over everything else. |
| `default` | string | Used when no other source provides one. |
| `from_command` | string | Shell command; trimmed stdout becomes the value. |
| `required` | bool | Resolution failure errors instead of falling through. |
| `base` | string | Integer-typed shorthand. Parsed as `i64`. |
| `offset` | string | Name of an env var whose value is added to `base`. |

Resolution order per key (first match wins): `value` → `base + offset`
→ pre-existing process / dotenv value → `from_command` → `default`.

## `[command.<name>]` (recipes)

Declarative commands. See [Recipes and Scripts](./Recipes-and-Scripts.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `desc` | string | unset | One-line description; shown by `keel list` and the TUI. |
| `run` | string \| `[string]` | required | Single command or sequential step list. |
| `in` | string | unset | Compose service to exec into. Absent = host. |
| `tty` | bool | `false` | Allocate a pseudo-TTY (`-it`). Required for shells. |
| `env` | table | `{}` | Per-recipe env overrides; merged last. |
| `needs` | `[string]` | `[]` | Other recipes that must succeed first. |
| `forward_args` | bool | `false` | Append CLI tail args to the command. |
| `parallel` | bool | `false` | Steps in `run = [...]` run concurrently. |
| `profile.<name>` | table | `{}` | Profile overrides; activated by `--profile <name>`. |

```toml
[command.test]
desc         = "Run test suite"
needs        = ["up"]
in           = "app"
run          = "composer test"
forward_args = true

[command.test.profile.ci]
tty = false
env = { XDEBUG_MODE = "off" }
```

### `[command.<name>.profile.<profile>]`

Optional override layer. Each field is optional; missing fields
inherit the recipe's value. Activated with `keel --profile <name> <recipe>`.

## `[hooks]`

Native keel git hooks. See [Hooks](./Hooks.md).

```toml
[hooks]
pre-commit = ["check:format", "check:lint"]
pre-push   = ["test"]
```

Each value is a list of recipe / script names to run for that stage.
External `.pre-commit-config.yaml` repos coexist; both are run by the
installed shim.

## `[install]`

First-time setup. See [Install Flow](./Install-Flow.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `steps` | `[InstallStepRef]` | `[]` | Ordered plan; when empty, `.keel/install/*` drives. |
| `install_git_hooks` | bool | `true` | Append a synthetic `install-hooks` step. |
| `gitignore` | string | `.keel/.gitignore` | Path of the auto-managed gitignore. |

`InstallStepRef` is either a name (resolves to a `.keel/install/`
file or a `[command.*]` recipe) or an inline table:

```toml
[[install.steps]]
name        = "seed-db"
run         = "php artisan db:seed"
optional    = true
interactive = false
in          = "app"             # optional; runs in container
cwd         = "./backend"       # optional; relative to project root
env         = { APP_ENV = "local" }
```

## `[worktrees]`

Per-worktree isolation. See [Worktrees](./Worktrees.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `modulus` | u32 | `1000` | Hash range for offsets. |
| `seed` | string | `[project].name` | Hash seed prefix. |
| `isolate_compose` | bool | `true` | Auto-set `COMPOSE_PROJECT_NAME = <project>-<slug>`. |
| `assign` | table\<string, u32\> | `{}` | Pin specific slugs to specific offsets. |
| `dotenv` | string | unset | Materialise resolved `[env]` into this dotenv file. |

```toml
[worktrees]
modulus = 100
dotenv  = ".env"

[worktrees.assign]
main       = 0
production = 0
```

## `[devcontainer]`

Opt-in devcontainer integration. See [Devcontainer](./Devcontainer.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | When true, recipes without `in =` and the TUI new-shell sentinel route into the devcontainer. |
| `path` | string | unset | Override auto-detect. Falls back to `.devcontainer/devcontainer.json`, then `.devcontainer.json`. |

```toml
[devcontainer]
enabled = true
```

## `[diff]`

Diff-view trunk override. See [Diff View](./Diff-View.md).

| Key | Type | Default | Notes |
|---|---|---|---|
| `base` | string | auto-detect | Branch name used as the diff base. |

## `[[services.custom]]` / `[[services.systemd]]`

Non-container services. See [Non Container Services](./Non-Container-Services.md).

```toml
[[services.custom]]
name   = "ngrok"
status = "pgrep -x ngrok"          # exit 0 = Running
start  = "ngrok http 8080 > /tmp/ngrok.log 2>&1 &"
stop   = "pkill -x ngrok"
restart = "..."                    # optional; defaults to stop && start
logs    = "tail -f /tmp/ngrok.log" # optional
desc    = "Public tunnel"          # optional

[[services.systemd]]
name  = "postgres"
unit  = "postgresql.service"
scope = "user"                     # "user" (default) | "system"
```

## `[agents]` and `[[agents.sources]]`

Upstream-sourced agent instructions. See [Agents](./Agents.md).

| `[agents]` | Type | Default | Notes |
|---|---|---|---|
| `install_with_setup` | bool | `true` | Apply during `keel install`. |
| `manifest_path` | string | `keel-agents.toml` | Default upstream manifest path. |

| `[[agents.sources]]` | Type | Notes |
|---|---|---|
| `name` | string | Unique source identifier. |
| `repo` | string | Git URL (or filesystem path). |
| `rev` | string | Tag, branch, or full SHA. |
| `subpath` | string | Optional clone subpath. |
| `manifest_path` | string | Per-source manifest path override. |
| `overrides` | `[MappingOverride]` | Per-mapping skip / relocate. |

```toml
[agents]
install_with_setup = true

[[agents.sources]]
name = "baseline"
repo = "https://github.com/acme/agent-baseline"
rev  = "v1.4.0"

[[agents.sources.overrides]]
dest   = "AGENTS.md"
action = "skip"

[[agents.sources.overrides]]
dest     = ".claude/skills/security-review.md"
relocate = ".claude/skills/security-review.upstream.md"
```

## `[ui]`

Optional TUI customisation. See [TUI](./TUI.md).

```toml
[ui]
default = "service:app"            # which pane is focused on launch

[[ui.pane]]
type    = "service"
service = "app"
key     = "1"

[[ui.pane]]
type        = "watcher"
glob        = ["src/**"]
on_change   = "test"
debounce_ms = 500
```

Pane `type` is one of `service`, `command`, or `watcher`; each
variant has its own field set.
