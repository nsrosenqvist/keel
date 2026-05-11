# devcontainer example

A project that opts into running terminal sessions and host-targeted
recipes inside a [devcontainer](https://containers.dev/).

```sh
cd examples/devcontainer
scaffl doctor          # reports devcontainer status + container plan
scaffl greet           # runs `uname -a` inside the devcontainer
scaffl shell           # interactive shell inside the devcontainer
scaffl ui              # press `n` in the Terminals view → docker exec
```

How it's wired:

```toml
# scaffl.toml
[devcontainer]
enabled = true
```

```jsonc
// .devcontainer/devcontainer.json
{
    "name": "scaffl-devcontainer-example",
    "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
    "workspaceFolder": "/workspaces/devcontainer-example",
    "remoteEnv": { "EDITOR": "vim" }
}
```

Requires docker on `PATH`. The container is built / started lazily on
the first `scaffl` command that needs it; subsequent commands just
`docker exec` into the running container.

See [docs/Devcontainer.md](../../docs/Devcontainer.md) for the
supported `devcontainer.json` subset and lifecycle details.
