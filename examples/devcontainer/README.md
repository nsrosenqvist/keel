# devcontainer example

A project that opts into running terminal sessions and host-targeted
recipes inside a [devcontainer](https://containers.dev/).

```sh
cd examples/devcontainer
croft doctor          # reports devcontainer status + container plan
croft greet           # runs `uname -a` inside the devcontainer
croft shell           # interactive shell inside the devcontainer
croft ui              # press `n` in the Terminals view → docker exec
```

How it's wired:

```toml
# croft.toml
[devcontainer]
enabled = true
```

```jsonc
// .devcontainer/devcontainer.json
{
    "name": "croft-devcontainer-example",
    "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
    "workspaceFolder": "/workspaces/devcontainer-example",
    "remoteEnv": { "EDITOR": "vim" }
}
```

Requires docker on `PATH`. The container is built / started lazily on
the first `croft` command that needs it; subsequent commands just
`docker exec` into the running container.

See [docs/Devcontainer.md](../../docs/Devcontainer.md) for the
supported `devcontainer.json` subset and lifecycle details.
