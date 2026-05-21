# AGENTS.md

Instructions for AI agents (and humans) working on this repository.

## North Star

**croft is the dev workspace you tend — it learns the shape of
your project, instead of demanding your project match it.**

Every developer ends up writing a `dev` shell script per project: it
preflights containers, routes commands between host and container,
holds recurring tasks (`up`, `shell`, `test`, `migrate`, `check`),
and forwards args to in-container tooling. The script grows,
refactors, and never quite leaves the repo it was born in. Tools like
DDEV solve this by enforcing a rigid format. Tools like `just` or
`mprocs` tend a slice and stop.

croft is the small plot where all of that lives together: a single
binary that defines commands two ways (declarative TOML *or* plain
scripts), knows where they should run (host vs service via a Backend
abstraction), doubles as a TUI dashboard, and keeps dev setup, git
hooks, and agent instructions on the same ground — with
`.pre-commit-config.yaml` compatibility so you don't have to abandon
the tools you already use.

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
  CLI runs it. The TUI runs it. Both go through `croft::runtime`.
- **No dead config.** Every option in `croft.toml` must change
  observable behaviour, or it doesn't ship.

## Deep-dive docs live in the wiki

End-user documentation, configuration / commands reference, and
per-subsystem deep-dives live in the
[**croft wiki**](https://github.com/nsrosenqvist/croft/wiki),
auto-synced from [`docs/`](./docs/) on every push to `main`. The
on-disk `docs/` tree is the source of truth — edit there, the wiki
updates.

Most relevant for an agent making code changes:

- [Architecture](https://github.com/nsrosenqvist/croft/wiki/Architecture)
  — operating principles in full, crate layout, dependency graph,
  cross-context patterns (value objects, the `Backend` trait,
  managed blocks).
- [Contributing](https://github.com/nsrosenqvist/croft/wiki/Contributing)
  — verification ladder, commit conventions, how to add a new
  example, PR conventions.
- [Configuration Reference](https://github.com/nsrosenqvist/croft/wiki/Configuration-Reference)
  and [Commands Reference](https://github.com/nsrosenqvist/croft/wiki/Commands-Reference)
  — every `croft.toml` key and every CLI subcommand.
- Subsystem pages: [Hooks](https://github.com/nsrosenqvist/croft/wiki/Hooks),
  [Agents](https://github.com/nsrosenqvist/croft/wiki/Agents),
  [Install Flow](https://github.com/nsrosenqvist/croft/wiki/Install-Flow),
  [Worktrees](https://github.com/nsrosenqvist/croft/wiki/Worktrees),
  [TUI](https://github.com/nsrosenqvist/croft/wiki/TUI),
  [Diff View](https://github.com/nsrosenqvist/croft/wiki/Diff-View).

When you change a feature, update the relevant wiki page in the same
PR. Each page cites the source file(s) it describes.

## When in doubt

- Default to fewer features, smaller surface area, sharper traits.
- A single integration test that runs against a real fixture beats
  five mocked unit tests.
- If a change makes `croft` slower for the common case, it does not
  ship without a measurement.
- Read `croft.toml` semantics conservatively: silent inference is a
  bug.
