# Shell Library

Interactive prompt helpers callable from any shell script. The
prompt goes to **stderr**, the answer to **stdout** — so capture
patterns like `EMAIL=$(ampelos lib ask "Email")` work out of the box.

## Why it's part of ampelos

Install scripts and recipes often need to ask the user for
something: an admin email, a yes/no confirmation, a multi-select
from compose services. Existing tools (`dialog`, `whiptail`,
`gum`) require an extra dep. `ampelos lib` ships in the binary
you're already using, with one consistent CLI shape and CI-friendly
defaults.

## Verbs

| Verb | Purpose | Output |
|---|---|---|
| `ask` | Single-line text input | answer to stdout |
| `confirm` | Yes/no | exit code (0 = yes, 1 = no) |
| `password` | No-echo text input | answer to stdout |
| `select` | Pick one or many from a list | one selection per line |
| `filter` | Fuzzy single-pick | answer to stdout |

## `ampelos lib ask`

```sh
EMAIL=$(ampelos lib ask "Email" --default "alice@example.com")
```

| Flag | Notes |
|---|---|
| `--default <STR>` | Used when stdin is non-tty or the user hits Enter. |

## `ampelos lib confirm`

```sh
if ampelos lib confirm "Seed the database?" --default yes; then
  php artisan db:seed
fi
```

| Flag | Notes |
|---|---|
| `--default yes\|no` | Used when stdin is non-tty or the user hits Enter. |

Exit codes: 0 = yes, 1 = no. No stdout output.

## `ampelos lib password`

```sh
PASS=$(ampelos lib password "Database password")
```

No echo, no `--default` (passwords have no sensible default).

## `ampelos lib select`

```sh
SERVICE=$(ampelos lib select "Pick a service" app db redis)

# Multi-select; one selection per line.
ampelos lib select "Pick services" --multi app db redis cache | \
  while read -r svc; do
    docker compose restart "$svc"
  done

# Read choices from a file or stdin.
ampelos lib select "Branch" --from <(git branch --format='%(refname:short)')
```

| Flag | Notes |
|---|---|
| `--multi` | Allow multiple selections. |
| `--default <IDX>` | Default-selected index (single-select only). |
| `--from <FILE>` | Read choices from a file, or `-` for stdin. |

## `ampelos lib filter`

```sh
PICK=$(ampelos lib filter "Branch" --from <(git branch --format='%(refname:short)'))
```

Fuzzy single-pick. Same I/O contract as single-select.

## CI / non-tty behaviour

Every verb honours `--default` (where applicable) when stdin is not
a tty. The same script that prompts a developer interactively runs
unattended in CI:

```sh
EMAIL=$(ampelos lib ask "Email" --default "ci@example.com")
```

`confirm` exits with the default code when there's no tty.
`password` errors when there's no tty (refusing to echo a default
secret to stdout).

## Use inside install steps

Install steps marked `# @interactive: yes` get the terminal for the
duration of the step, so `ampelos lib *` prompts work without an IPC
sentinel:

```sh
#!/usr/bin/env bash
# @desc: Configure first-run secrets
# @interactive: yes
EMAIL=$(ampelos lib ask "Admin email")
PASS=$(ampelos lib password "Admin password")
echo "ADMIN_EMAIL=$EMAIL" >> .env
echo "ADMIN_PASS=$PASS"   >> .env
```

See [Install Flow](Install-Flow#interactive-steps).

## See also

- [Install Flow](Install-Flow) for the `# @interactive:`
  frontmatter.
- [`examples/install-flow/`](https://github.com/nsrosenqvist/ampelos/tree/main/examples/install-flow)
  for a runnable demo using `ampelos lib ask` inside an install
  step.
