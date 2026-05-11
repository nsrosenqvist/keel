# Hooks

keel owns a small git-hook installer plus a native runner that
understands `.pre-commit-config.yaml`. No dependency on the
`pre-commit` binary; keel runs supported hook languages itself,
errors loudly on the rest.

## Two hook sources

keel recognises hooks from two places, both run by the same shim:

1. **`[hooks.<stage>]` in `keel.toml`** — native keel hooks.
   Each value is a list of recipe / script names.

   ```toml
   [hooks]
   pre-commit = ["check:format", "check:lint"]
   pre-push   = ["test"]
   ```

2. **`.pre-commit-config.yaml`** at the project root — standard
   pre-commit-format file, parsed by keel directly. Both
   `repo: local` and external repos work as long as their
   `language` resolves to `system` or `script`.

## Installing the shims

```sh
keel hooks install               # default: pre-commit
keel hooks install --stages pre-commit,pre-push,post-merge
```

Each stage gets a `.git/hooks/<stage>` shim:

```sh
#!/usr/bin/env sh
# managed by keel
exec keel hooks run <stage> "$@"
```

Existing non-keel hooks at the same path are left alone — the
installer refuses to overwrite a foreign hook (with a clear error)
to avoid clobbering a custom git-hook setup. If you've moved an old
hook out of the way, re-running `keel hooks install` writes the
shim cleanly.

`keel hooks uninstall [--stages ...]` removes only keel-managed
shims (identified by the marker comment).

## Implicit stages

When `[worktrees].dotenv = "..."` is set, `keel hooks install`
without an explicit `--stages` list also installs `post-checkout`
and `post-merge` shims so the dotenv writer keeps the file fresh
across branch switches even when the developer skips keel. See
[Worktrees](./Worktrees.md#materialising-worktree-env-into-env).

## External pre-commit repos

External repos in `.pre-commit-config.yaml` are cloned into
`.keel/cache/hooks/<slug(url)-rev>/` by `keel-hooks/src/cache.rs`
(via the shared
[`keel-cache`](https://github.com/nsrosenqvist/keel/tree/main/crates/keel-cache)
crate). The runner reads `.pre-commit-hooks.yaml` inside the clone
to find each hook's `entry` and `language`, merges with the user's
`HookSpec`, and runs it natively.

```yaml
# .pre-commit-config.yaml
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

`keel install --update-hooks` force-refreshes the cache (clears
the entry, re-clones at the same rev). Useful when an upstream
moves a tag.

## What languages run natively

keel runs hooks whose effective language is `system` or `script`:

- **`system`** — the `entry` is invoked as-is via the shell. The
  tool must already be on `PATH`.
- **`script`** — the `entry` resolves to a script file inside the
  cached repo (or the project for `repo: local`); keel exec's it
  directly.

Anything else (`python`, `node`, `ruby`, `golang`, …) errors at
install / run time with a clear message:

```
hook `flake8` uses language `python`; keel runs only `system` /
`script` hooks. Use a wrapper script with `language: script`, or a
tool already on PATH with `language: system`.
```

`repo: meta` (pre-commit's built-in hooks like `check-hooks-apply`,
`identity`) errors the same way — those aren't implemented in
keel.

## Running hooks manually

`keel hooks run <stage>` is what the installed shim invokes; you
can run it directly to debug:

```sh
keel hooks run pre-commit             # uses staged files
keel hooks run pre-push origin main   # forwards stage args
```

Output streams to your terminal; non-zero exit on the first failing
hook unless `always_run = true` opts a hook out of staged-file
filtering.

## Resolution model

For a given stage, keel runs:

1. Every `[hooks.<stage>]` entry from `keel.toml`, in order, via
   the recipe runner.
2. Every `.pre-commit-config.yaml` hook whose stages include
   `<stage>` (or whose `default_stages` does), in declaration order.

Both lists run sequentially; the first non-zero exit halts the
shim. Set `always_run = true` on a hook to make it run regardless
of which files are staged.

## See also

- [Install Flow](./Install-Flow.md) — `keel install` includes a
  synthetic step that installs hook shims and prefetches external
  repos.
- [`examples/hooks/`](https://github.com/nsrosenqvist/keel/tree/main/examples/hooks)
  — runnable demo with a local hook and an external repo.
- [Configuration Reference: `[hooks]`](./Configuration-Reference.md#hooks).
