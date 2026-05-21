# Example: agents-upstream

A self-describing upstream layout that an org can fork to ship
shared agent instructions and skills via `croft agents`.

## Layout

```
croft-agents.toml      # the manifest croft reads downstream
agents/                 # whole-file destinations
  CLAUDE.md             #   → CLAUDE.md (whole-file ownership)
  AGENTS.md             #   → AGENTS.md (mode = once: written once, then yours)
skills/                 # → .claude/skills/ (one file per skill)
  conventional-commits.md
  run-checks.md
commands/               # → .claude/commands/ (one file per command)
  croft-doctor.md
```

## Consume from a downstream project

Add to `croft.toml`:

```toml
[[agents.sources]]
name = "org-baseline"
repo = "https://github.com/your-org/agent-baseline"
rev  = "v1.0.0"
```

Then run:

```sh
croft agents install      # pull pinned upstream into the project
croft agents update       # re-resolve revs (auto-refetches floating refs)
croft agents status       # per-source pinned rev + per-file drift
croft agents diff         # what would change without writing
```

`croft install` runs an `apply-agents` synthetic step before
`install-hooks` automatically when this section is present
(`[agents].install_with_setup` defaults to true).

## Local overrides

Per the whole-file-ownership rule, croft never edits files it
writes. Local additions go in sibling files:

- For `CLAUDE.md` / `AGENTS.md` — drop content into a
  `CLAUDE.local.md` and the upstream `CLAUDE.md` ends with a
  Claude-Code-style `@CLAUDE.local.md` import.
- For `.claude/skills/` (a `[[dir]]` target) — local skills coexist
  in the same directory; croft tracks ownership in
  `.croft/agents.state.json` and only updates the files it owns.
- A local file with the same name as an upstream file in a
  `[[dir]]` target is a `LocalShadow` error; rename it (e.g.
  `foo.local.md`) to disambiguate.

## Skip or relocate an upstream mapping

```toml
[[agents.sources.overrides]]
dest   = "AGENTS.md"
action = "skip"

[[agents.sources.overrides]]
dest     = ".claude/skills/run-checks.md"
relocate = ".claude/skills/run-checks.upstream.md"
```
