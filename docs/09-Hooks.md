# Hooks

`croft hooks` runs checks at git lifecycle points — typically
formatters and linters before a commit, tests before a push.
Same effect as the `pre-commit` Python tool, but built in: no
extra binary to install, no virtualenvs created in your tree,
and every hook can route into a service container or
devcontainer the same way recipes do.

## Quickstart

**1. Pick what to run** in `croft.toml`:

```toml
[hooks]
pre-commit = ["check:format", "check:lint"]
pre-push   = ["test"]
```

The strings are recipe / script names — anything `croft <name>`
runs, a hook can run. (`check:format` and `check:lint` here are
ordinary `[command.*]` recipes elsewhere in your `croft.toml`.)

**2. Install the git shims:**

```sh
croft hooks install
```

This writes `.git/hooks/pre-commit` and `pre-push` that delegate
to `croft hooks run <stage>`. Foreign hooks at the same paths are
left alone (with a clear error) so you don't lose a hand-written
setup.

**3. Commit. The hook fires.** If a step exits non-zero the
commit is aborted, same as plain git.

To run a stage manually for debugging:

```sh
croft hooks run pre-commit
```

## Mental model

- **Two sources, one runner.** croft looks at `[hooks.<stage>]` in
  `croft.toml` *and* at `.pre-commit-config.yaml` if it exists.
  Both are evaluated for the firing stage and run sequentially;
  the first non-zero exit halts.
- **Every hook's `entry` runs verbatim.** croft doesn't manage
  toolchains the way the `pre-commit` binary does. If your
  `.pre-commit-config.yaml` says `entry: ruff`, croft runs `ruff`
  — make sure it's on `PATH` (typically as part of `croft install`).
  The `language:` field is parsed and remembered but never gates
  execution.
- **Hooks route like recipes.** A hook with `in: "<service>"`
  execs inside that container service. Otherwise — when
  `[devcontainer] enabled = true` — it runs inside the
  devcontainer. Otherwise, on the host.

## Common tasks

### Block commits unless format + lint pass

```toml
[command.check]
run = ["check:format", "check:lint"]

[hooks]
pre-commit = ["check"]
```

A single recipe wraps the checks; the hook just calls it.

### Use an existing `.pre-commit-config.yaml`

If your repo already has one, croft reads it as-is — both
`repo: local` hooks and external repos (`repo: https://...`)
work:

```yaml
repos:
  - repo: local
    hooks:
      - id: rustfmt
        name: rustfmt
        language: system
        entry: cargo fmt --all -- --check
        files: \.rs$

  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v4.5.0
    hooks:
      - id: trailing-whitespace
      - id: end-of-file-fixer
```

External repos are cloned into `.croft/cache/hooks/<slug-rev>/` on
first use and reused thereafter. `croft install --update-hooks`
force-refreshes the cache.

### Run a hook inside a service container

Add `in: <service>` to any hook in `.pre-commit-config.yaml`
(croft extension; plain pre-commit ignores it):

```yaml
- repo: local
  hooks:
    - id: phpstan
      name: phpstan
      language: system
      entry: vendor/bin/phpstan analyse
      in: app                    # exec inside the `app` compose service
      pass_filenames: false
```

For native `[hooks.<stage>]` entries, routing comes from each
referenced recipe's own `in =` field — no separate switch needed.

### Hook a different stage

```sh
croft hooks install --stages pre-commit,pre-push,commit-msg,post-merge
```

Any of git's standard stages works. `croft hooks uninstall
[--stages ...]` removes only croft-managed shims (identified by
a marker comment).

### Use `language: python` / `node` / etc.

Declare them as you would in pre-commit:

```yaml
- repo: local
  hooks:
    - id: ruff
      language: python
      entry: ruff
      files: \.py$
```

croft runs `ruff` directly. Make sure it's installed — for
example, by adding an install step:

```sh
# .croft/install/40-tools.sh
#!/usr/bin/env bash
# @desc: Dev tooling
pip install --user ruff
```

The same applies inside a service container — install the tool
in the image.

### Auto-refresh `.env` after a branch switch

Setting `[worktrees].dotenv = ".env"` makes `croft hooks install`
auto-include `post-checkout` and `post-merge` shims (no need to
list them explicitly) so the dotenv file stays fresh when a
developer skips croft and runs `docker compose up` directly. See
[Worktrees](Worktrees#materialising-worktree-env-into-env).

## Where hooks run

For each hook, precedence is:

1. **`in: "<service>"`** on the hook → exec inside that container
   service via the configured `[runtime].backend`.
2. **`[devcontainer] enabled = true`** → hooks without `in` run
   inside the project's devcontainer.
3. **Otherwise** → host spawn, with cwd set to the git repo root.

`in` is a croft extension to `.pre-commit-config.yaml` — plain
`pre-commit` ignores it, so the same config works for either
tool.

## Resolution order

For a given stage, hooks run in this order, first non-zero exit
halts:

1. Every `[hooks.<stage>]` entry from `croft.toml`, in declaration
   order, via the recipe runner.
2. Every `.pre-commit-config.yaml` hook whose `stages` includes
   `<stage>` (or whose `default_stages` does), in declaration
   order.

Set `always_run = true` on a hook to make it run regardless of
which files are staged.

## Configuration reference

### `croft.toml`

```toml
[hooks]
pre-commit = ["check:format", "check:lint"]
pre-push   = ["test"]
post-merge = ["refresh-deps"]
```

Each key is a stage name; each value is a list of recipe / script
names. Routing comes from the referenced recipe's own `in =`.

### `.pre-commit-config.yaml`

Standard pre-commit format, plus the croft-only `in:` extension
on hook entries. Every hook's `entry` is parsed with
`shell_words` and spawned directly; `language:` is advisory.

The one shape croft still rejects is `repo: meta` (pre-commit's
built-in `check-hooks-apply` / `identity` etc.) — there's no
`entry` to dispatch.

## Command reference

| Command | Notes |
|---|---|
| `croft hooks install [--stages ...]` | Write `.git/hooks/<stage>` shims. Default: `pre-commit` (plus `post-checkout`/`post-merge` if `[worktrees].dotenv` is set). |
| `croft hooks uninstall [--stages ...]` | Remove only croft-managed shims. |
| `croft hooks run <stage> [args...]` | What the shim invokes. Run directly to debug. |
| `croft install --update-hooks` | Force-refresh cached external pre-commit repos. |

## See also

- [Install Flow](11-Install-Flow) — `croft install` includes a
  synthetic step that installs hook shims and prefetches external
  repos.
- [`examples/hooks/`](https://github.com/nsrosenqvist/croft/tree/main/examples/hooks)
  — runnable demo with a local hook and an external repo.
- [Configuration Reference: `[hooks]`](Configuration-Reference#hooks).
