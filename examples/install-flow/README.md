# Example: install-flow

A runnable walk-through of `croft install`'s ordered-step model.
Four numbered scripts under `.croft/install/` exercise the common
shapes: regular, interactive, optional, and a final summary step.

## Run it

From inside this directory (`examples/install-flow/`):

```sh
croft install
```

The renderer redraws each step's row in place
(◐ running → ✓ ok / ✗ failed / → skipped). The final step prints a
ready banner.

State lands in `.croft/install.state.json`. Re-running prompts
"Resume from `<step>`?" when a step previously failed; `--resume`
bypasses the prompt, `--restart` wipes state.

## What each step demonstrates

| File | Feature |
|---|---|
| `01-copy-env` | Idempotent first-write of a starter `.env`. |
| `02-collect-admin` | `# @interactive: yes` plus `croft lib ask --default` for CI parity. |
| `03-seed-db` | `# @optional: yes` — non-zero exit is recorded as `skipped` and the plan continues. |
| `04-finalize` | A plain step (no flags) that summarises the run. |

The marker files under `state/` (created by each step) make it
visible which steps ran, including across `--restart` and resume.

## Try the flags

```sh
croft install --list
```

Plan plus last-known status per step. Useful when a new step lands
in `.croft/install/` and a maintainer wants every teammate to run
just that one:

```sh
croft install 03-seed-db
```

Single-step mode updates only that step's record; the rest stay
where they were.

Force the optional step to fail to see how `@optional` behaves:

```sh
INSTALL_FLOW_FORCE_SEED_FAIL=1 croft install --restart
```

`03-seed-db` exits non-zero, gets marked `skipped`, and the plan
continues. With `@optional: no`, the same failure would halt the
plan and trigger the resume prompt on the next invocation.

Wipe state and start fresh:

```sh
croft install --restart
```

## Pointers

- [Install Flow](../../docs/Install-Flow.md) — the full model.
- [Shell Library](../../docs/Shell-Library.md) — every `croft lib`
  prompt usable inside an `@interactive` step.
- [Configuration Reference: `[install]`](../../docs/Configuration-Reference.md#install).
