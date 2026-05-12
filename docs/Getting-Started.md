# Getting Started

This page takes you from "no keel" to "first recipe + first
hook + open dashboard" in under five minutes. After that, the
[Quick Tour](Quick-Tour) shows the rest of the surface.

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

This drops a `keel.toml` at the project root and walks a registry of
ecosystem detectors against the directory. Each detector contributes
to the generated file:

| Detector       | Trigger files                                                  | What it contributes                                       |
| -------------- | -------------------------------------------------------------- | --------------------------------------------------------- |
| `compose`      | `docker-compose.{yml,yaml}`, `compose.{yml,yaml}`              | `[runtime] backend = "compose"`                           |
| `devcontainer` | `.devcontainer/devcontainer.json`, `.devcontainer.json`        | `[devcontainer] enabled = true`                           |
| `dotenv`       | `.env`, `.env.local`                                           | `[env_files]` entries                                     |
| `node`         | `package.json`, `deno.json[c]`, plus lockfile selects the tool | `dev` / `build` / `test` / `lint` using `npm` / `pnpm` / `yarn` / `bun` / `deno` |
| `python`       | `uv.lock`, `poetry.lock`, `pdm.lock`, `Pipfile.lock`, `requirements.txt`, `pyproject.toml` | `install` / `test` / (`lint`) using `uv` / `poetry` / `pdm` / `pipenv` / `pip` |
| `rust`         | `Cargo.toml` (workspace-aware)                                 | `build` / `test` / `fmt` / `check`                        |
| `go`           | `go.mod`                                                       | `build` / `test` / `run` / `vet`                          |
| `ruby`         | `Gemfile`; Rails via `bin/rails` or `config/application.rb`    | `install` / `test`; Rails adds `console` / `migrate`      |
| `php`          | `composer.json`; Laravel via `artisan`; Symfony via `symfony.lock` | `install` / `test`; Laravel adds `artisan` / `migrate`, Symfony adds `console` |

Open the file — every detected command is **commented**. Uncomment
the ones you want to keep. When two detectors suggest the same name
(e.g. both `node` and `rust` want `build`), `init` emits both under a
"Multiple ecosystems suggest …" header so you can pick one.

Pass `--template <NAME>` to skip auto-detection and start from a
hand-curated stack scaffold instead (`laravel`, `rails`, `node`,
`rust`).

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
automatically. See [Hooks](Hooks) for the full model
(including `.pre-commit-config.yaml` compatibility).

## 5. Open the dashboard

```sh
keel
```

Bare `keel` opens the [TUI](TUI): a sidebar of every recipe,
script, and service; an output pane for whatever's selected; service
lifecycle keymaps; a built-in [diff view](Diff-View) (`G`); a
[worktree switcher](Worktrees#tui-worktree-switcher-w) (`W`).

## 6. Where to go next

- **The full tour** → [Quick Tour](Quick-Tour) — a guided 5-minute walk
  through every major feature.
- **Schema reference** →
  [Configuration Reference](Configuration-Reference) for
  every `keel.toml` key.
- **Real projects** → [Examples](Examples) — runnable
  configurations under `examples/`.
- **Stuck?** → [Troubleshooting](Troubleshooting) — `keel
  doctor` and common pitfalls.
