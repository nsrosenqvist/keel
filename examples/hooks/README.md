# Example: hooks

Git hook configuration end to end. Native `[hooks]` recipes from
`scaffl.toml` plus a `.pre-commit-config.yaml` mixing `repo: local`
hooks and an external repo at a pinned tag — all run by the same
installed shim.

## Prerequisites

A git repository. From this directory:

```sh
git init -q -b main
git add -A && git commit -q -m "seed"
```

(or copy the example into another git repo and run from there).

## Install the shims

```sh
scaffl hooks install
```

Writes `.git/hooks/pre-commit` (and any other stages you declare).
Each shim is a one-liner:

```sh
#!/usr/bin/env sh
# managed by scaffl
exec scaffl hooks run pre-commit "$@"
```

Existing non-scaffl hooks at the same path are left alone — the
installer refuses to overwrite a foreign hook with a clear error.
Move your old hook aside (e.g. `.git/hooks/pre-commit.bak`) and
rerun `scaffl hooks install`.

## What runs on `git commit`

For the `pre-commit` stage, the installed shim runs (in order):

1. **Native scaffl hooks** from `[hooks.pre-commit]` in
   `scaffl.toml`. In this example: `check`, which expands to
   `fmt` then `lint`.
2. **`.pre-commit-config.yaml` hooks** whose stages include
   `pre-commit`. In this example: `trim-trailing-ws`,
   `no-tabs-in-yaml` (local), then `end-of-file-fixer`,
   `check-yaml`, `check-added-large-files` (from the pinned
   external repo).

First non-zero exit halts the shim, exactly as `pre-commit` would.

## External hook caching

The first invocation clones
`https://github.com/pre-commit/pre-commit-hooks` at `v4.5.0` into:

```
.scaffl/cache/hooks/<slug(url)-v4.5.0>/clone/
.scaffl/cache/hooks/<slug(url)-v4.5.0>/meta.json
```

Subsequent runs reuse the cache. To force a refresh (e.g. when an
upstream moves a tag):

```sh
scaffl install --update-hooks
```

## Try the manual run path

You can run hooks without going through git:

```sh
scaffl hooks run pre-commit
```

Same execution as the installed shim invokes. Useful for debugging
which hook fails.

## Uninstall

```sh
scaffl hooks uninstall
```

Removes only scaffl-managed shims (identified by the marker
comment). Foreign hooks at the same paths are left alone.

## Errors you can trip on purpose

Add an unsupported language to `.pre-commit-config.yaml`:

```yaml
- id: flake8
  language: python
  entry: flake8
```

`scaffl hooks install` errors with:

> hook `flake8` uses language `python`; scaffl runs only `system`
> / `script` hooks. Use a wrapper script with `language: script`,
> or a tool already on PATH with `language: system`.

Same shape for `repo: meta` (pre-commit's built-in hooks like
`check-hooks-apply`, `identity`).

## Pointers

- [Hooks](../../docs/Hooks.md) — the full subsystem doc.
- [Configuration Reference: `[hooks]`](../../docs/Configuration-Reference.md#hooks).
- [Install Flow](../../docs/Install-Flow.md) — `scaffl install`
  bundles a synthetic `install-hooks` step that does the same job
  as the manual `scaffl hooks install` command above.
