# Architecture

scaffl is a Rust workspace of focused crates, each one a bounded
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
  `scaffl-runtime`.
- **No dead config.** Every option in `scaffl.toml` must change
  observable behaviour, or it doesn't ship.

## Crate map

| Crate | Bounded context |
|---|---|
| `scaffl-cli` | Binary; clap; subcommand dispatch. The only crate that knows about `clap`. |
| `scaffl-config` | TOML / YAML parsing; schema; env resolution. No I/O orchestration. |
| `scaffl-runtime` | Recipe resolver; executor; output sinks; preflight. |
| `scaffl-container` | `Backend` trait; compose / docker / podman / null impls. |
| `scaffl-tui` | Embedded ratatui dashboard; stateful `App`; pure render fn. |
| `scaffl-cache` | Content-addressed git cache shared by hooks + agents. |
| `scaffl-hooks` | `.pre-commit-config.yaml` reader; native runner; git hook shim installer. |
| `scaffl-agents` | Upstream-sourced agent instructions / skills pipeline. |

```
crates/
  scaffl-cli/        # binary; clap; subcommand dispatch
  scaffl-config/     # TOML / YAML parsing; schema; env resolution
  scaffl-runtime/    # recipe resolution; supervision; preflight
  scaffl-container/  # Backend trait; compose / docker / podman impls
  scaffl-tui/        # ratatui app; panes; palette
  scaffl-cache/      # content-addressed git cache shared by hooks + agents
  scaffl-hooks/      # .pre-commit-config.yaml reader; git hook installer
  scaffl-agents/     # upstream-sourced agent instructions / skills pipeline
examples/            # runnable scaffl projects
docs/                # this wiki, synced via .github/workflows/wiki-sync.yml
```

## Layering

The dependency graph is a DAG (no cycles). From "leaves" to
"trunk":

```
scaffl-cache  →  scaffl-hooks   ↘
                                  →  scaffl-cli
scaffl-cache  →  scaffl-agents  ↗

scaffl-config →  scaffl-runtime →  scaffl-cli
              ↘  scaffl-tui     ↗
scaffl-container →  scaffl-runtime
```

`scaffl-cli` is the only crate that imports everything else. The
TUI and the CLI are different views of the same runtime, never two
implementations of the same logic.

## Cross-context patterns

### Value objects across crates

Each context defines its own concrete types and exposes value
objects (no behaviour, no `&mut self`) for travel between contexts.
Example: `scaffl-cache::RepoRef` is the input shape both
`scaffl-hooks` and `scaffl-agents` translate their domain types
into; the cache crate stays unaware of either consumer's `Repo` /
`SourceSpec`.

### Backend trait

`scaffl-container::Backend` is the only abstraction `scaffl-runtime`
depends on. Implementations live in `scaffl-container` (`compose`,
`docker`, `podman`, `null`) and a custom backend in
`scaffl-container::custom` for `[[services.systemd]]` /
`[[services.custom]]` declarations.

### Idempotent managed blocks

`scaffl-config::managed_block` writes a marker-delimited section
into a file (used by the worktree dotenv writer and the
`.scaffl/.gitignore` writer). The block is replaced in place on
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
- If a change makes `scaffl` slower for the common case, it does
  not ship without a measurement.
- Read `scaffl.toml` semantics conservatively: silent inference is
  a bug.

## See also

- [Contributing](./Contributing.md) — verification ladder, commit
  conventions, how to add a new example.
