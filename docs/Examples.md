# Examples

Runnable scaffl projects under
[`examples/`](https://github.com/nsrosenqvist/scaffl/tree/main/examples)
in the source tree. Clone the repo, `cd` into one, and `scaffl
list` will show what's wired up.

## [`minimal`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/minimal)

The smallest useful `scaffl.toml`: a couple of host-only recipes,
no containers, no services. Use as a starting template when you're
adding scaffl to a project that doesn't need compose.

Showcases:

- `[command.*]` recipe basics.
- `[env]` resolution without container backends.
- `[containers].backend = "none"`.

## [`laravel-app`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/laravel-app)

A complete Laravel + Docker Compose project, modeled on the
hand-rolled `dev` scripts scaffl was built to replace. Covers most
of the surface in one place.

Showcases:

- Compose backend with `default_service = "app"`.
- Recipes that exec inside containers (`scaffl artisan migrate`,
  `scaffl shell`, `scaffl test --filter Login`).
- Native `[hooks]` plus a `.pre-commit-config.yaml` mix.
- Watch mode pinned in the TUI as a pane.
- `[worktrees].dotenv = ".env"` so two checkouts run side-by-side
  with distinct port ranges.

## [`install-flow`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/install-flow)

`.scaffl/install/` ordered setup with optional + interactive steps.
Walks through `scaffl install`, single-step mode (`scaffl install
<step>`), `--restart`, `--list`, and how `install.state.json`
drives resume.

Showcases:

- Numeric-prefix ordered steps (`01-copy-env`, `02-install-deps`,
  `03-seed-db`, `04-finalize`).
- `# @optional: yes` for steps that may fail in some environments.
- `# @interactive: yes` plus `scaffl lib ask` for first-run secret
  prompts.
- Resume-after-failure prompt and `--restart`.

## [`hooks`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/hooks)

Git hook configuration end to end: native `[hooks]` recipes plus a
`.pre-commit-config.yaml` mixing `repo: local` (`language: system`)
and an external repo at a pinned tag.

Showcases:

- `scaffl hooks install` writes `.git/hooks/pre-commit`.
- The shim format and what gets executed.
- `.scaffl/cache/hooks/<rev>/` cache for the external repo.
- The `repo: meta` / unsupported-language error messages and how
  to wrap a non-`system` hook with a `language: script` shim.

## [`devcontainer`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/devcontainer)

Opt-in devcontainer integration: `[devcontainer] enabled = true`
plus a minimal image-only `.devcontainer/devcontainer.json`. Use as
a starting template when you want `scaffl shell` and TUI terminals
to land inside an isolated workspace container instead of the host.

Showcases:

- `[devcontainer]` config block.
- Auto-detect of `.devcontainer/devcontainer.json`.
- `remoteEnv` injected into every `docker exec`.
- `scaffl doctor` reporting the resolved container + image plan.

## [`agents-upstream`](https://github.com/nsrosenqvist/scaffl/tree/main/examples/agents-upstream)

A self-describing upstream layout an org can fork to ship shared
agent instructions and skills via `[[agents.sources]]`.

Showcases:

- `scaffl-agents.toml` upstream manifest with `[[file]]`,
  `[[dir]]`, and `mode = "once"`.
- `agents/CLAUDE.md`, `agents/AGENTS.md`, `skills/`,
  `commands/` directory layout.
- Local sibling overrides (`CLAUDE.local.md`).
- Downstream override examples (skip / relocate).

## See also

- [Getting Started](./Getting-Started.md) — first project from
  scratch.
- [Quick Tour](./Quick-Tour.md) — a guided walk-through that
  references each example.
