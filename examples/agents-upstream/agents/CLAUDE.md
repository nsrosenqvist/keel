# Project agent instructions

This file is sourced from the org-wide agent baseline. Local
project additions belong in `CLAUDE.local.md` next to this file —
Claude Code reads both when it sees an `@CLAUDE.local.md` import
directive at the bottom of this file.

## Working agreements

- Conventional Commits only.
- Keep changes scoped: one concern per PR.
- Run `cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` before each commit.

@CLAUDE.local.md
