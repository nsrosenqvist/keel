# Agents

If your team keeps a shared set of agent instructions and skills
(`CLAUDE.md`, `AGENTS.md`, `.claude/skills/*.md`, …) in a git repo
somewhere, `ampelos agents` syncs them into the current project. You
subscribe to an upstream by pointing at its repo + revision; ampelos
clones it, copies the files it owns, and tracks them in
`.ampelos/agents.state.json` so it can update them cleanly later.

Same spirit as `ampelos hooks`: declarative config, local cache, no
third-party tool in the loop.

## Quickstart

**1. Point at an upstream** in `ampelos.toml`:

```toml
[[agents.sources]]
name = "baseline"
repo = "https://github.com/acme/agent-baseline"
rev  = "v1.4.0"
```

**2. Apply:**

```sh
ampelos agents install
```

ampelos clones the upstream into `.ampelos/cache/agents/`, reads its
`ampelos-agents.toml`, and writes whatever files it declares
(`CLAUDE.md`, `.claude/skills/*.md`, etc.) to your project. Safe
to re-run — it's idempotent.

**3. Verify:**

```sh
ampelos agents status
```

That's the whole loop. From now on, `ampelos install` includes a
synthetic `apply-agents` step too, so fresh clones get agent
files materialised automatically.

## Mental model

Three rules cover almost every situation.

- **Upstream owns the bytes.** Every file ampelos writes is a
  byte-for-byte copy from the upstream repo. If you want to
  customise something, put it in a *sibling* file — e.g. add
  `CLAUDE.local.md` next to a ampelos-owned `CLAUDE.md`. ampelos never
  touches files it didn't write.
- **State is the source of truth.** `.ampelos/agents.state.json`
  (per-checkout, gitignored) records every file ampelos wrote with a
  sha256. That's how it knows what to update, what's been
  hand-edited (drift), and what's now an orphan.
- **One pipeline for everything.** `install`, `update`, `diff`, and
  the synthetic step inside `ampelos install` all run the same apply
  pipeline with different flags. No separate "set up" / "tear
  down" semantics to keep straight.

## Common tasks

### Skip a file you don't want

Use a per-source override, keyed on the upstream's `dest`:

```toml
[[agents.sources.overrides]]
dest   = "AGENTS.md"
action = "skip"
```

Next `ampelos agents install` won't write it. If ampelos had written it
before, it's removed as an orphan.

### Move a file somewhere else

```toml
[[agents.sources.overrides]]
dest     = ".claude/skills/security-review.md"
relocate = ".claude/skills/security-review.upstream.md"
```

Useful when you want both the upstream version (relocated) and a
local version (at the original path).

### Pull in updates

```sh
ampelos agents update                     # all sources
ampelos agents update --source baseline   # just one
```

If your `rev` is a pinned SHA or semver tag (`v1.4.0`), bump it in
`ampelos.toml` first. Floating refs like `main` auto-refetch on every
`update` — no bump needed.

### See what's been edited

```sh
ampelos agents status
```

Reports per-source revisions and any *drift* — ampelos-owned files
whose disk content no longer matches what ampelos last wrote. Drift
is left alone by default (you might be testing a change). To
re-overwrite from upstream:

```sh
ampelos agents install --force-overwrite-drift
```

### Add a second upstream

Stack another `[[agents.sources]]` block. If two sources claim the
same file, the **later-declared one wins**; the losers show up in
`status` so you can decide whether to add a `skip` / `relocate`.

### Stop using a source

Delete its `[[agents.sources]]` block from `ampelos.toml` and run
`ampelos agents install`. Every file the removed source owned is
deleted as an orphan; empty `.claude/skills/` subdirs are pruned
(but `.claude/` itself is kept).

### Preview a change first

```sh
ampelos agents install --dry-run
ampelos agents diff
```

Both show the actions a real apply would take, neither writes.

## Authoring an upstream

If you maintain the org-wide agents repo, drop a `ampelos-agents.toml`
at its root:

```toml
[[file]]
src  = "agents/CLAUDE.md"
dest = "CLAUDE.md"

[[file]]
src  = "agents/AGENTS.md"
dest = "AGENTS.md"
mode = "once"            # write only if dest absent

[[dir]]
src  = "skills/"
dest = ".claude/skills/"
glob = "**/*.md"          # optional; defaults to "**/*"
```

`mode = "once"` is for seed files the project takes ownership of
after first install (a starter `AGENTS.md` is the canonical case).
ampelos won't overwrite a `once` file and won't warn on drift against
it, but does remove it if the mapping disappears.

A starter layout lives at
[`examples/agents-upstream/`](https://github.com/nsrosenqvist/ampelos/tree/main/examples/agents-upstream).

## Configuration reference

```toml
[agents]
install_with_setup = true                 # default; runs apply during `ampelos install`
manifest_path      = "ampelos-agents.toml"   # default upstream filename

[[agents.sources]]
name    = "baseline"
repo    = "https://github.com/acme/agent-baseline"
rev     = "v1.4.0"
# subpath = "claude/"                     # optional, monorepo support

[[agents.sources.overrides]]
dest   = "AGENTS.md"
action = "skip"

[[agents.sources.overrides]]
dest     = ".claude/skills/security-review.md"
relocate = ".claude/skills/security-review.upstream.md"
```

Override match key is the upstream-declared `dest` (post-`subpath`,
pre-merge) — i.e. the path you actually see in your tree.

## Command reference

| Command | Notes |
|---|---|
| `ampelos agents install` | Apply pinned upstream sources. Idempotent. |
| `ampelos agents install --force` | Re-clone every source, ignore cache. |
| `ampelos agents install --dry-run` | Plan without writing. |
| `ampelos agents install --force-overwrite-drift` | Overwrite hand-edited ampelos-owned files. |
| `ampelos agents update [--source NAME]...` | Re-resolve revs and re-apply. Floating refs auto-refetch. |
| `ampelos agents status [--strict]` | Per-source rev + per-file drift. `--strict` exits 1 on drift. |
| `ampelos agents diff` | Print the actions a fresh apply would take. |

## Edge cases worth knowing

**Local-shadow conflicts.** If a hand-written file already sits at
a path an upstream wants to write (e.g. your `.claude/skills/foo.md`
collides with a newly-added upstream `foo.md`), `ampelos agents
install` errors with a rename suggestion (`foo.local.md`). The
rule keeps the resolution explicit: ampelos never silently overwrites
a file it didn't author.

**Cross-source collisions** are resolved by **declaration order**
in `ampelos.toml` — the later source wins. `status` lists the
overshadowed sources so you can disambiguate explicitly with an
override if you want.

**Floating refs** are anything that isn't a 7–40 char hex SHA or a
semver-shaped tag (`v1.2.3`, `1.2.3-rc.1`). Branch names like
`main`, `develop`, `HEAD`, or ambiguous strings like a bare `v1`
count as floating, and `ampelos agents update` re-fetches them every
time (the cache is bypassed for those sources only). Pin to a SHA
or semver tag if you want reproducible installs.

**Drift vs. orphans.**

- *Drift* — file in state, on disk, content differs from what
  ampelos wrote. Left alone by default; `--force-overwrite-drift`
  restores from upstream.
- *Orphan* — file in state but the current resolved set no longer
  claims it (source removed, mapping skipped, upstream renamed).
  Removed on next apply.

## See also

- [Install Flow](Install-Flow) — how `apply-agents` slots into the
  install plan.
- [Configuration Reference: `[agents]`](Configuration-Reference#agents-and-agentssources).
