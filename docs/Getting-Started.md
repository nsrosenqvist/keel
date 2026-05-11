# Getting Started

This page takes you from "no keel" to "first recipe + first
hook + open dashboard" in under five minutes. After that, the
[Quick Tour](./Quick-Tour.md) shows the rest of the surface.

## 1. Install

keel is a single Rust binary. Two install paths:

### From a clone

```sh
git clone https://github.com/nsrosenqvist/keel
cd keel
cargo install --path .
```

The binary is named `keel` and lands in `~/.cargo/bin/`.

### Once published to crates.io

```sh
cargo install keel
```

(Pre-alpha; not yet on crates.io. Track the project README for the
publish announcement.)

### Verify

```sh
keel --version
```

## 2. Generate a starter `keel.toml`

Inside any project directory:

```sh
keel init
```

This drops a `keel.toml` at the project root with detection hints
based on what `init` finds in the directory (compose, `.env`,
`package.json`, `composer.json`). Open the file — every recipe is
commented and ready for you to uncomment / rename.

Pass `--template <NAME>` to skip auto-detection
(`keel init --template minimal`).

## 3. Define your first recipe

Edit `keel.toml` to add a recipe for a command you actually run:

```toml
[command.up]
desc = "Start all services"
run  = "docker compose up -d"

[command.shell]
desc = "Open a shell in the app container"
in   = "app"
run  = "/bin/sh"
tty  = true
```

`in = "app"` execs inside the named compose service. Absent → host.
`tty = true` allocates a pseudo-TTY (required for shells).

Run them:

```sh
keel up
keel shell
```

`keel list` (or `keel ls`) shows every recipe and script, with
their descriptions.

## 4. Install your first git hook

Add `[hooks]` to `keel.toml`:

```toml
[command.check]
desc = "Format + lint"
run  = ["check:format", "check:lint"]

[command.check:format]
run = "cargo fmt -- --check"

[command.check:lint]
run = "cargo clippy --all-targets -- -D warnings"

[hooks]
pre-commit = ["check"]
```

Then:

```sh
keel hooks install
```

This writes `.git/hooks/pre-commit` as a shim that runs `keel
hooks run pre-commit`. Try it: `git commit` triggers `check`
automatically. See [Hooks](./Hooks.md) for the full model
(including `.pre-commit-config.yaml` compatibility).

## 5. Open the dashboard

```sh
keel
```

Bare `keel` opens the [TUI](./TUI.md): a sidebar of every recipe,
script, and service; an output pane for whatever's selected; service
lifecycle keymaps; a built-in [diff view](./Diff-View.md) (`G`); a
[worktree switcher](./Worktrees.md#tui-worktree-switcher-w) (`W`).

## 6. Where to go next

- **The full tour** → [Quick Tour](./Quick-Tour.md) — a guided 5-minute walk
  through every major feature.
- **Schema reference** →
  [Configuration Reference](./Configuration-Reference.md) for
  every `keel.toml` key.
- **Real projects** → [Examples](./Examples.md) — runnable
  configurations under `examples/`.
- **Stuck?** → [Troubleshooting](./Troubleshooting.md) — `keel
  doctor` and common pitfalls.
