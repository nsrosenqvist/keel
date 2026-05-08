# AGENTS.md

Instructions for AI agents (and humans) working on this repository.

## North Star

**scaffl is the dev-loop wrapper that adapts to your project, instead of forcing your project to adapt to it.**

Every developer ends up writing a `dev` shell script per project: it preflights containers, routes commands between host and container, wraps recurring tasks (`up`, `shell`, `test`, `migrate`, `check`), and forwards args to in-container tooling. The script grows, refactors, and never quite leaves the repo it was born in. Tools like DDEV solve this by enforcing a rigid format. Tools like `just` or `mprocs` solve a slice and stop.

scaffl is the union: a single binary that

1. **Defines commands two ways** — declaratively in `scaffl.toml` *or* as plain scripts under `.scaffl/commands/`. Use whichever shape matches the command's complexity.
2. **Knows where commands run** — host or service, via a Backend abstraction. Compose first; podman/docker pluggable.
3. **Doubles as a TUI dashboard** where you attach to service logs *and trigger commands* — not just a log viewer.
4. **Handles dev setup and git hooks**, with `.pre-commit-config.yaml` compatibility so projects can adopt scaffl without abandoning their existing hook ecosystem.

The user-visible promise: `scaffl init` in any Compose project produces a working dev loop in under a minute, and replacing your hand-rolled `dev` script with a `scaffl.toml` is a strict win, not a sideways move.

The architectural promise: each capability lives in its own crate with a focused trait surface. The CLI and the TUI are different views of the same runtime, never two implementations of the same logic.

## Operating principles

These shape every code change in the repo:

- **SOLID.** Single-responsibility crates; backends and runners depend on traits, not concretes; small, focused interfaces.
- **DDD.** Bounded contexts split as crates: `config`, `runtime`, `container`, `hooks`, `tui`. Cross-context types travel through well-defined value objects, not shared mutable state.
- **Performance is a default, not an afterthought.** Stream output, don't buffer it. Prefer `&str` and `Cow<'_, str>` over owning `String` where lifetimes allow. Avoid clone-happy code. No runtime reflection — TOML schemas are serde-derived at compile time.
- **One source of truth per concern.** A recipe is defined once. The CLI runs it. The TUI runs it. Both go through `scaffl-runtime`.
- **No dead config.** Every option in `scaffl.toml` must change observable behaviour, or it doesn't ship.

## Required verification step

After **every** code change, run the full verification ladder before committing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If any step fails, fix the cause — don't suppress warnings, don't `#[allow]` clippy lints without a written justification in the commit body, and don't `--no-verify` past pre-commit hooks.

For UI / TUI changes specifically: also run a manual smoke test (`cargo run -- ui` against the example project under `examples/`) before reporting the change as complete.

## Commits — Conventional Commits

All commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<optional body explaining the why>

<optional footers, e.g. BREAKING CHANGE:>
```

**Types** used in this repo: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `build`, `ci`, `perf`.

**Scopes** match crate or area: `config`, `runtime`, `container`, `tui`, `hooks`, `cli`, `workspace`, `docs`.

**Subject**: imperative, lowercase, no trailing period. Under 70 characters.

**Body**: explain *why*, not *what*. The diff already shows the what.

Examples:

- `feat(config): support env var expansion in run strings`
- `fix(runtime): propagate non-zero exit codes from compose exec`
- `refactor(container): extract Backend trait from compose impl`
- `perf(runtime): stream stdout instead of buffering before printing`
- `docs(agents): clarify when to use scripts vs recipes`

Breaking changes: add `BREAKING CHANGE: <description>` as a footer, and add `!` after type/scope: `feat(config)!: rename run_args to forward_args`.

## Layout

```
crates/
  scaffl-cli/        # binary; clap; subcommand dispatch
  scaffl-config/     # TOML / YAML parsing; schema; env resolution
  scaffl-runtime/    # recipe resolution; supervision; preflight
  scaffl-container/  # Backend trait; compose / docker / podman impls
  scaffl-tui/        # ratatui app; panes; palette
  scaffl-hooks/      # .pre-commit-config.yaml reader; git hook installer
examples/            # fixture projects used by integration tests
```

The full design plan lives in the original plan file referenced in the project README; this document supersedes any contradiction.

## When in doubt

- Default to fewer features, smaller surface area, sharper traits.
- A single integration test that runs against a real fixture beats five mocked unit tests.
- If a change makes `scaffl` slower for the common case, it does not ship without a measurement.
- Read `scaffl.toml` semantics conservatively: silent inference is a bug.
