# AGENTS.md

Instructions for AI agents (and humans) working on this repository.

## North Star

**keel is the dev-loop wrapper that adapts to your project, instead
of forcing your project to adapt to it.**

Every developer ends up writing a `dev` shell script per project: it
preflights containers, routes commands between host and container,
wraps recurring tasks (`up`, `shell`, `test`, `migrate`, `check`),
and forwards args to in-container tooling. The script grows,
refactors, and never quite leaves the repo it was born in. Tools like
DDEV solve this by enforcing a rigid format. Tools like `just` or
`mprocs` solve a slice and stop.

keel is the union: a single binary that defines commands two ways
(declarative TOML *or* plain scripts), knows where they should run
(host vs service via a Backend abstraction), doubles as a TUI
dashboard, and handles dev setup + git hooks + agent instructions
with `.pre-commit-config.yaml` compatibility for adoption without
abandoning existing ecosystems.

## Required verification step

After **every** code change, run the full verification ladder
**before** committing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

All four steps gate CI — running any subset locally is not enough.
The `cargo doc` step in particular catches broken intra-doc-links
and private-item link warnings that the other three miss.

If any step fails, fix the cause — don't suppress warnings, don't
`#[allow]` clippy lints without a written justification in the
commit body, and don't `--no-verify` past pre-commit hooks.

For UI / TUI changes specifically: also run a manual smoke test
(`cargo run -- ui` against an example project under `examples/`)
before reporting the change as complete.

## Commits — Conventional Commits

All commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<optional body explaining the why>

<optional footers, e.g. BREAKING CHANGE:>
```

**Types** used in this repo: `feat`, `fix`, `refactor`, `docs`,
`test`, `chore`, `build`, `ci`, `perf`.

**Scopes** match crate or area: `config`, `runtime`, `container`,
`tui`, `hooks`, `agents`, `cache`, `cli`, `workspace`, `docs`.

**Subject**: imperative, lowercase, no trailing period. Under 70
characters.

**Body**: explain *why*, not *what*. The diff already shows the
what.

Breaking changes: add `BREAKING CHANGE: <description>` as a footer,
and add `!` after type/scope:
`feat(config)!: rename run_args to forward_args`.

## Operating principles

- **SOLID.** Single-responsibility crates; backends and runners
  depend on traits, not concretes.
- **DDD.** Bounded contexts split as crates. Cross-context types
  travel through value objects, not shared mutable state.
- **Performance is a default, not an afterthought.** Stream output,
  don't buffer. Prefer `&str` and `Cow<'_, str>` where lifetimes
  allow. No runtime reflection — TOML schemas are serde-derived at
  compile time.
- **One source of truth per concern.** A recipe is defined once. The
  CLI runs it. The TUI runs it. Both go through `keel::runtime`.
- **No dead config.** Every option in `keel.toml` must change
  observable behaviour, or it doesn't ship.

## Deep-dive docs live in the wiki

End-user documentation, configuration / commands reference, and
per-subsystem deep-dives live in the
[**keel wiki**](https://github.com/nsrosenqvist/keel/wiki),
auto-synced from [`docs/`](./docs/) on every push to `main`. The
on-disk `docs/` tree is the source of truth — edit there, the wiki
updates.

Most relevant for an agent making code changes:

- [Architecture](https://github.com/nsrosenqvist/keel/wiki/Architecture)
  — operating principles in full, crate layout, dependency graph,
  cross-context patterns (value objects, the `Backend` trait,
  managed blocks).
- [Contributing](https://github.com/nsrosenqvist/keel/wiki/Contributing)
  — verification ladder, commit conventions, how to add a new
  example, PR conventions.
- [Configuration Reference](https://github.com/nsrosenqvist/keel/wiki/Configuration-Reference)
  and [Commands Reference](https://github.com/nsrosenqvist/keel/wiki/Commands-Reference)
  — every `keel.toml` key and every CLI subcommand.
- Subsystem pages: [Hooks](https://github.com/nsrosenqvist/keel/wiki/Hooks),
  [Agents](https://github.com/nsrosenqvist/keel/wiki/Agents),
  [Install Flow](https://github.com/nsrosenqvist/keel/wiki/Install-Flow),
  [Worktrees](https://github.com/nsrosenqvist/keel/wiki/Worktrees),
  [TUI](https://github.com/nsrosenqvist/keel/wiki/TUI),
  [Diff View](https://github.com/nsrosenqvist/keel/wiki/Diff-View).

When you change a feature, update the relevant wiki page in the same
PR. Each page cites the source file(s) it describes.

## When in doubt

- Default to fewer features, smaller surface area, sharper traits.
- A single integration test that runs against a real fixture beats
  five mocked unit tests.
- If a change makes `keel` slower for the common case, it does not
  ship without a measurement.
- Read `keel.toml` semantics conservatively: silent inference is a
  bug.
