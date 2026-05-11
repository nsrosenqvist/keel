# Agents

`keel agents` manages agent instructions and skills (`CLAUDE.md`,
`AGENTS.md`, `.claude/skills/`, `.claude/commands/`, …) sourced from
upstream git repos. Same spirit as the hook subsystem — declarative,
locally cached, no third-party tool in the loop.

## The pipeline

1. **Cache.** `keel::cache::clone_or_reuse` (the same primitive the
   hooks subsystem uses) caches the upstream at
   `.keel/cache/agents/<slug(url)-rev>/`. Floating refs (anything
   that isn't a 7–40 hex SHA or a semver-shaped tag) auto-refetch on
   every `keel agents update`; pinned refs reuse the cache.
2. **Manifest.** The upstream's `keel-agents.toml` declares
   `[[file]]` and `[[dir]]` mappings (src → dest, optional `mode =
   "once"`).
3. **Override.** Downstream `[[agents.sources.overrides]]` patches
   mappings by their upstream-declared `dest` (skip / relocate).
4. **Apply.** The pipeline computes write / update / remove /
   unchanged actions against `.keel/agents.state.json` and writes
   via temp-file-and-rename. State tracks every file with a SHA-256
   for drift detection and orphan removal.
5. **Synthetic install step.** `keel install` runs an
   `apply-agents` step before `install-hooks` when
   `[agents].install_with_setup = true` (the default) and at least
   one source is declared.

## Whole-file ownership

Every file keel writes is byte-for-byte from upstream. Local
overrides go in **sibling** files (e.g. `CLAUDE.local.md` next to a
keel-owned `CLAUDE.md`); keel never touches them.

For directory targets, local files coexist in the same target dir
with upstream files. Keel only manages files it wrote (tracked in
state); local files are left alone. A non-state file in a `[[dir]]`
target with the same name as an upstream file is a `LocalShadow`
error with a rename suggestion — tell the user to rename the local
file so the conflict is unambiguous.

## Downstream config (`keel.toml`)

```toml
[agents]
install_with_setup = true                   # default
manifest_path      = "keel-agents.toml"   # default

[[agents.sources]]
name    = "baseline"
repo    = "https://github.com/acme/agent-baseline"
rev     = "v1.4.0"
# subpath = "claude/"                       # optional, monorepo support

[[agents.sources.overrides]]
dest   = "AGENTS.md"
action = "skip"

[[agents.sources.overrides]]
dest     = ".claude/skills/security-review.md"
relocate = ".claude/skills/security-review.upstream.md"

[[agents.sources]]
name = "rust-skills"
repo = "https://github.com/acme/rust-agents"
rev  = "main"                               # floating; auto-refetched on update
```

Override match key is the upstream-declared `dest` (post-`subpath`,
pre-merge) — that's the path the user actually sees in their tree.

## Upstream manifest (`keel-agents.toml`)

Lives at the upstream repo's root (or a subpath, if `subpath` is
set):

```toml
[[file]]
src  = "agents/CLAUDE.md"
dest = "CLAUDE.md"

[[file]]
src  = "agents/AGENTS.md"
dest = "AGENTS.md"
mode = "once"            # write only if dest absent; never overwrite

[[dir]]
src  = "skills/"
dest = ".claude/skills/"
glob = "**/*.md"          # optional; defaults to "**/*"
```

`mode` defaults to `"replace"`. `"once"` is the only carve-out from
strict whole-file ownership — useful for seed files (e.g. starter
`AGENTS.md`) the project takes ownership of after first write.
State records `once` files with `sha256: null` so the drift scan
skips them but orphan removal still works if the source disappears.

## Commands

| Command | Notes |
|---|---|
| `keel agents install` | Apply pinned upstream sources. Idempotent. |
| `keel agents install --force` | Re-clone every source, ignore cache. |
| `keel agents install --dry-run` | Plan without writing. |
| `keel agents install --force-overwrite-drift` | Overwrite hand-edited keel-owned files. |
| `keel agents update [--source NAME]...` | Re-resolve revs and re-apply. Floating refs auto-refetch. |
| `keel agents status [--strict]` | Per-source rev + per-file drift. `--strict` exits 1 on drift. |
| `keel agents diff` | Print actions a fresh apply would take. |

## State + drift

`.keel/agents.state.json` is the source of truth for what keel
owns:

```json
{
  "version": 1,
  "applied_at_ms": 1715000000000,
  "sources": [
    {
      "name": "baseline",
      "repo": "https://github.com/acme/agent-baseline",
      "rev_request": "v1.4.0",
      "resolved_sha": "abc123...",
      "manifest_sha256": "def456..."
    }
  ],
  "files": [
    {
      "dest": "CLAUDE.md",
      "source_name": "baseline",
      "src": "agents/CLAUDE.md",
      "sha256": "<sha of bytes keel wrote, or null for mode=once>",
      "mode": "replace",
      "written_at_ms": 1715000000000
    }
  ]
}
```

The state file is gitignored — it's per-checkout, not shared.

**Drift** is a keel-owned file whose disk content hashes to
something other than what we last wrote. By default, drift is
left alone (warned in the report); `--force-overwrite-drift`
overwrites it.

**Orphans** are files in state that the current resolved set no
longer claims (source removed, mapping skipped, upstream renamed).
They're removed on next apply, with empty parent dirs pruned up to
but not including `.claude/`.

## Cross-source collisions

When two sources resolve the same `dest`, the **later-declared**
source wins. The losers are listed in the report so you can add an
override to disambiguate explicitly.

## Floating refs

`is_floating_rev(rev)` flags any rev that isn't a 7–40 char hex SHA
or a semver-shaped tag (`v1.2.3`, `1.2.3-rc.1`, …) as floating.
Branch names like `main`, `develop`, `HEAD` count as floating, as do
ambiguous strings like a bare `v1`. Floating refs auto-refetch on
every `keel agents update` (the cache is bypassed for those
sources).

## See also

- [`examples/agents-upstream/`](https://github.com/nsrosenqvist/keel/tree/main/examples/agents-upstream)
  — sample upstream layout an org can fork.
- [Install Flow](./Install-Flow.md) — how `apply-agents` slots into
  the install plan.
- [Configuration Reference: `[agents]`](./Configuration-Reference.md#agents-and-agentssources).
