# Contributing

ampelos is an open-source dev-loop wrapper. Contributions —
features, fixes, docs, examples — are welcome. This page captures
the non-negotiables.

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

## Conventional Commits

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

Examples:

- `feat(config): support env var expansion in run strings`
- `fix(runtime): propagate non-zero exit codes from compose exec`
- `refactor(container): extract Backend trait from compose impl`
- `perf(runtime): stream stdout instead of buffering before printing`
- `docs: clarify when to use scripts vs recipes`

Breaking changes: add `BREAKING CHANGE: <description>` as a
footer, and add `!` after type/scope:
`feat(config)!: rename run_args to forward_args`.

## Design philosophy

See [Architecture](20-Architecture) for the operating principles
(SOLID, DDD, performance is a default, one source of truth per
concern, no dead config). Every code change is judged against those.

## Adding a new example

Examples live under
[`examples/`](https://github.com/nsrosenqvist/ampelos/tree/main/examples).
Each example is a runnable ampelos project plus a short README that
calls out which features it demonstrates.

1. `mkdir examples/<name>` and add a `ampelos.toml` plus any
   supporting files (`.pre-commit-config.yaml`, `.ampelos/install/*`,
   etc.).
2. Add a `README.md` linking back to the relevant wiki page(s) and
   explaining what the example shows.
3. Update [`docs/18-Examples.md`](18-Examples) with a one-paragraph
   summary and links.
4. Open the PR with a `docs(examples): add <name> example` commit.

## Adding a new feature

Sketch:

1. **Design.** Open a discussion or draft PR with the schema
   change + the user-facing behaviour.
2. **Implement.** Land the feature behind well-bounded crate
   surfaces. Cross-crate types travel as value objects.
3. **Document.** Add or update the relevant
   [`docs/`](https://github.com/nsrosenqvist/ampelos/tree/main/docs)
   wiki page. The wiki is auto-synced from `docs/` on push to `main`.
4. **Test.** At least one integration test against a real fixture
   under `examples/` or a `tempfile::TempDir`.

## Opening a pull request

- Branch off `main` (`git switch -c feat/whatever`).
- One concern per PR. Long-running features can land in stages
  behind a flag, but each merge should leave the tree in a working
  state.
- The PR description should explain *why* — what problem the
  change solves and which design alternatives you considered.
- The verification ladder must be green before you mark the PR
  ready for review.

## See also

- [Architecture](20-Architecture) — crate layout + design
  principles.
- [Examples](18-Examples) — existing examples to model from.
