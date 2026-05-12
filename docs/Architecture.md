# Architecture

ampelos is a Rust workspace of focused crates, each one a bounded
context. Cross-context types travel through value objects; no
shared mutable state.

## Operating principles

These shape every code change:

- **SOLID.** Single-responsibility crates; backends and runners
  depend on traits, not concretes; small, focused interfaces.
- **DDD.** Bounded contexts split as crates: `config`, `runtime`,
  `container`, `hooks`, `agents`, `cache`, `tui`. Cross-context
  types travel through well-defined value objects, not shared
  mutable state.
- **Performance is a default, not an afterthought.** Stream output,
  don't buffer it. Prefer `&str` and `Cow<'_, str>` over owning
  `String` where lifetimes allow. Avoid clone-happy code. No
  runtime reflection — TOML schemas are serde-derived at compile
  time.
- **One source of truth per concern.** A recipe is defined once.
  The CLI runs it. The TUI runs it. Both go through
  `ampelos::runtime`.
- **No dead config.** Every option in `ampelos.toml` must change
  observable behaviour, or it doesn't ship.

## Module map

Each top-level directory under `src/` is a bounded context.

| Module | Bounded context |
|---|---|
| `ampelos::cli` | Binary; clap; subcommand dispatch. The only module that knows about `clap`. |
| `ampelos::config` | TOML / YAML parsing; schema; env resolution. No I/O orchestration. |
| `ampelos::runtime` | Recipe resolver; executor; output sinks; preflight. |
| `ampelos::container` | `Backend` trait; compose / docker / podman / null impls. |
| `ampelos::tui` | Embedded ratatui dashboard; stateful `App`; pure render fn. |
| `ampelos::cache` | Content-addressed git cache shared by hooks + agents. |
| `ampelos::hooks` | `.pre-commit-config.yaml` reader; native runner; git hook shim installer. |
| `ampelos::agents` | Upstream-sourced agent instructions / skills pipeline. |

```
src/
  main.rs        # thin wrapper → ampelos::cli::run
  lib.rs         # exposes every bounded-context module
  cli/           # binary; clap; subcommand dispatch
  config/        # TOML / YAML parsing; schema; env resolution
  runtime/       # recipe resolution; supervision; preflight
  container/     # Backend trait; compose / docker / podman impls
  tui/           # ratatui app; panes; palette
  cache/         # content-addressed git cache shared by hooks + agents
  hooks/         # .pre-commit-config.yaml reader; git hook installer
  agents/        # upstream-sourced agent instructions / skills pipeline
examples/        # runnable ampelos projects
docs/            # this wiki, synced via .github/workflows/wiki-sync.yml
```

## Layering

The dependency graph between modules is a DAG (no cycles). From
"leaves" to "trunk":

```
cache  →  hooks    ↘
                       →  cli
cache  →  agents   ↗

config →  runtime  →  cli
        ↘  tui      ↗
container →  runtime
```

`ampelos::cli` is the only module that imports everything else. The
TUI and the CLI are different views of the same runtime, never two
implementations of the same logic.

The bounded contexts are enforced by directory + `pub(crate)` /
`pub(super)` visibility rather than crate boundaries — the project
ships as a single `ampelos` binary crate; the layering is a code-review
and refactor-discipline concern, not a compiler-enforced one.

## Cross-context patterns

### Value objects across modules

Each context defines its own concrete types and exposes value
objects (no behaviour, no `&mut self`) for travel between contexts.
Example: `ampelos::cache::RepoRef` is the input shape both
`ampelos::hooks` and `ampelos::agents` translate their domain types
into; the cache module stays unaware of either consumer's `Repo` /
`SourceSpec`.

### Backend trait

`ampelos::container::Backend` is the only abstraction `ampelos::runtime`
depends on. Implementations live in `ampelos::container` (`compose`,
`docker`, `podman`, `null`) and a custom backend in
`ampelos::container::custom` for `[[services.systemd]]` /
`[[services.custom]]` declarations.

### Idempotent managed blocks

`ampelos::config::managed_block` writes a marker-delimited section
into a file (used by the worktree dotenv writer and the
`.ampelos/.gitignore` writer). The block is replaced in place on
each write; user content above and below is preserved; identical
content is a no-op (mtime stays put).

## Required verification step

After **every** code change, run the full verification ladder
before committing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If any step fails, fix the cause — don't suppress warnings, don't
`#[allow]` clippy lints without a written justification in the
commit body, and don't `--no-verify` past pre-commit hooks.

For UI / TUI changes specifically: also run a manual smoke test
(`cargo run -- ui` against the example project under `examples/`)
before reporting the change as complete.

## When in doubt

- Default to fewer features, smaller surface area, sharper traits.
- A single integration test that runs against a real fixture beats
  five mocked unit tests.
- If a change makes `ampelos` slower for the common case, it does
  not ship without a measurement.
- Read `ampelos.toml` semantics conservatively: silent inference is
  a bug.

## See also

- [Contributing](Contributing) — verification ladder, commit
  conventions, how to add a new example.
