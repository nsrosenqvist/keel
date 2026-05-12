# Commands Reference

Every `keel <subcommand>` and its flags. Source of truth:
[`src/cli/app.rs`](https://github.com/nsrosenqvist/keel/blob/main/src/cli/app.rs).
Anything not matched by an explicit subcommand falls through to
recipe / script resolution, then to compose passthrough — see
[Recipes and Scripts](Recipes-and-Scripts).

## Global flags

| Flag | Notes |
|---|---|
| `--project <PATH>` | Project root override. Default: search upward from cwd. |
| `--explain` | Print the resolution path without executing. |
| `--profile <NAME>` | Activate a recipe profile (`[command.*.profile.<name>]`). |

## `keel` (no subcommand)

Bare invocation opens the [TUI dashboard](TUI). With a name and
args, resolves the name as: built-in → recipe → script → compose
passthrough → service exec → unknown.

## `keel list` / `keel ls`

Print every recipe and script as a table (name, kind, container,
description).

## `keel which <name>`

Show how `<name>` would resolve (recipe / script / compose / service /
unknown). Same logic as `--explain`, no execution.

## `keel env [--write PATH]`

Print the resolved project environment (`[env]` + dotenv layering +
worktree-derived values). With `--write PATH`, writes the same
content into `PATH` as a marker-delimited managed block — used by
the auto-write hook in `[worktrees].dotenv` and by post-checkout /
post-merge git hooks.

## `keel doctor`

Validate config + backend availability + env files + non-container
service status. Exits non-zero on any failure. See
[Troubleshooting](Troubleshooting).

## `keel init [--template <NAME>]`

Generate a starter `keel.toml` at the project root with detection
hints for the host stack. `--template` picks a stack explicitly
instead of auto-detecting (compose / docker / podman / minimal).

## `keel install [<step>] [flags]`

Run the install plan (`.keel/install/*` + `[install].steps`). See
[Install Flow](Install-Flow). With a positional `<step>`, runs
that step alone.

| Flag | Notes |
|---|---|
| `--resume` | Non-interactive resume from the first unresolved step. |
| `--restart` | Wipe state, run from step one. |
| `--dry-run` | Print the plan without executing. |
| `--list` | Plan + last-known status per step. |
| `--update-hooks` | Force-refresh the external hook cache. |

## `keel ui`

Open the [TUI dashboard](TUI) explicitly (same as bare `keel`
with no args).

## `keel shell [--service <name>]`

Drop into an interactive shell.

| Flag | Notes |
|---|---|
| (none) | Enter the project's devcontainer. Requires `[devcontainer] enabled = true`. Ensures the container is up first. |
| `--service <name>` | Enter the named compose service (`docker compose exec -it <name>`). Independent of devcontainer config. |

See [Devcontainer](Devcontainer).

## `keel hooks <action>`

| Action | Notes |
|---|---|
| `install [--stages s1,s2]` | Write `.git/hooks/<stage>` shims. Default: `pre-commit`. |
| `uninstall [--stages s1,s2]` | Remove keel-managed shims; default: every known stage. |
| `run <stage>` | Run hooks for `<stage>`. Used by the installed shims. |

See [Hooks](Hooks).

## `keel agents <action>`

| Action | Notes |
|---|---|
| `install [--force] [--dry-run] [--force-overwrite-drift]` | Apply pinned upstream sources. |
| `update [--source NAME]... [--force] [--dry-run] [--force-overwrite-drift]` | Re-resolve revs and re-apply. Floating refs auto-refetch. |
| `status [--strict]` | Per-source pinned rev + per-file drift. `--strict` exits non-zero on drift. |
| `diff` | Print actions a fresh apply would take. |

See [Agents](Agents).

## `keel watch <recipe> [args...] [flags]`

Re-run `<recipe>` whenever watched files change.

| Flag | Notes |
|---|---|
| `--path <PATH>` | Path to watch. Repeat for multiple. Default: project root. |
| `--debounce-ms <MS>` | Debounce window. Default: 300. |

See [Watch](Watch).

## `keel worktree <action>`

| Action | Notes |
|---|---|
| `status` | Current worktree's slug, offset, derived env. |
| `list` | Every git worktree + computed offset, with collision warnings. |
| `assign <slug> <n> [--local]` | Pin a slug to an offset. `--local` writes to `.keel/local.toml`. |

See [Worktrees](Worktrees).

## `keel lib <action>`

Interactive prompt helpers callable from any shell script. Prompt to
stderr, answer to stdout. Non-tty invocations honour `--default`.
See [Shell Library](Shell-Library).

| Action | Signature |
|---|---|
| `ask <prompt> [--default <STR>]` | Single-line text input. |
| `confirm <prompt> [--default yes\|no]` | Yes/no; exit 0 = yes, 1 = no. |
| `password <prompt>` | No echo. |
| `select <prompt> <choices...> [--multi] [--default <IDX>] [--from <FILE>]` | Pick one or many. |
| `filter <prompt> <choices...> [--from <FILE>]` | Fuzzy picker. |

## `keel completions <shell>`

Emit a shell-completion script. `<shell>` is one of `bash`, `zsh`,
`fish`, `elvish`, `powershell`. Pipe to your shell's completion
location.
