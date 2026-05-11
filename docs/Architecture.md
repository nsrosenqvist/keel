# Architecture

keel is a Rust workspace of focused crates, each one a bounded
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
  `keel-runtime`.
- **No dead config.** Every option in `keel.toml` must change
  observable behaviour, or it doesn't ship.

## Crate map

| Crate | Bounded context |
|---|---|
| `keel-cli` | Binary; clap; subcommand dispatch. The only crate that knows about `clap`. |
| `keel-config` | TOML / YAML parsing; schema; env resolution. No I/O orchestration. |
| `keel-runtime` | Recipe resolver; executor; output sinks; preflight. |
| `keel-container` | `Backend` trait; compose / docker / podman / null impls. |
| `keel-tui` | Embedded ratatui dashboard; stateful `App`; pure render fn. |
| `keel-cache` | Content-addressed git cache shared by hooks + agents. |
| `keel-hooks` | `.pre-commit-config.yaml` reader; native runner; git hook shim installer. |
| `keel-agents` | Upstream-sourced agent instructions / skills pipeline. |

```
crates/
  keel-cli/        # binary; clap; subcommand dispatch
  keel-config/     # TOML / YAML parsing; schema; env resolution
  keel-runtime/    # recipe resolution; supervision; preflight
  keel-container/  # Backend trait; compose / docker / podman impls
  keel-tui/        # ratatui app; panes; palette
  keel-cache/      # content-addressed git cache shared by hooks + agents
  keel-hooks/      # .pre-commit-config.yaml reader; git hook installer
  keel-agents/     # upstream-sourced agent instructions / skills pipeline
examples/            # runnable keel projects
docs/                # this wiki, synced via .github/workflows/wiki-sync.yml
```

## Layering

The dependency graph is a DAG (no cycles). From "leaves" to
"trunk":

```
keel-cache  →  keel-hooks   ↘
                                  →  keel-cli
keel-cache  →  keel-agents  ↗

keel-config →  keel-runtime →  keel-cli
              ↘  keel-tui     ↗
keel-container →  keel-runtime
```

`keel-cli` is the only crate that imports everything else. The
TUI and the CLI are different views of the same runtime, never two
implementations of the same logic.

## Cross-context patterns

### Value objects across crates

Each context defines its own concrete types and exposes value
objects (no behaviour, no `&mut self`) for travel between contexts.
Example: `keel-cache::RepoRef` is the input shape both
`keel-hooks` and `keel-agents` translate their domain types
into; the cache crate stays unaware of either consumer's `Repo` /
`SourceSpec`.

### Backend trait

`keel-container::Backend` is the only abstraction `keel-runtime`
depends on. Implementations live in `keel-container` (`compose`,
`docker`, `podman`, `null`) and a custom backend in
`keel-container::custom` for `[[services.systemd]]` /
`[[services.custom]]` declarations.

### Idempotent managed blocks

`keel-config::managed_block` writes a marker-delimited section
into a file (used by the worktree dotenv writer and the
`.keel/.gitignore` writer). The block is replaced in place on
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
- If a change makes `keel` slower for the common case, it does
  not ship without a measurement.
- Read `keel.toml` semantics conservatively: silent inference is
  a bug.

## See also

- [Contributing](./Contributing.md) — verification ladder, commit
  conventions, how to add a new example.
