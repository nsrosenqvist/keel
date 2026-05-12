# Devcontainer

keel can route terminal sessions and host-targeted recipes into a
project's [devcontainer](https://containers.dev/) instead of running
them on the host. The feature is **opt-in**: keel ignores
`devcontainer.json` until you flip it on.

```toml
[devcontainer]
enabled = true
# Optional: override auto-detect.
# path = ".devcontainer/devcontainer.json"
```

When enabled, keel auto-detects `.devcontainer/devcontainer.json`
first, then `.devcontainer.json`. Use `path` to point at a custom
location (e.g. when you keep multiple variants in one repo).

## What changes when it's on

| Path | Off (default) | On |
|---|---|---|
| TUI `n` / new shell | `$SHELL` on host | `docker exec` into devcontainer |
| Recipe without `in =` | host fork | runs inside devcontainer |
| Recipe with `in = "<svc>"` | unchanged | unchanged |
| `keel shell` | (no command) | enters devcontainer |
| `keel shell --service app` | (no command) | enters compose service `app` (devcontainer toggle irrelevant) |

Existing routing for `in = "<service>"` is **unaffected**. Devcontainer
and compose services are different targets: the compose backend keeps
managing `[services.app]`, and the devcontainer keeps being the
workspace shell. They coexist.

## Supported `devcontainer.json` fields (v1)

keel parses a subset of the spec — enough for the most common
"isolated workspace image" pattern:

- `name`
- `image` *or* `build: { dockerfile, context, args }`
- `workspaceFolder` (default: `/workspaces/<project-root-basename>`)
- `runArgs` (passed through to `docker run`; privilege-escalating
  entries log a warning)
- `containerEnv` (baked in at `docker run`)
- `remoteEnv` (merged on top of keel recipe env at `docker exec`)
- `remoteUser`

Comments and trailing commas in the JSON are accepted (the file
format is JSONC).

### Unsupported / rejected

- `dockerComposeFile` — **rejected** with an error. Use keel's
  existing `runtime.backend = "compose"` instead, and route
  recipes to your dev service with `in = "<service>"`. The two
  approaches solve the same problem; reimplementing
  dockerComposeFile would just duplicate the compose backend.
- `features`, `postCreateCommand`, `onCreateCommand`,
  `updateContentCommand`, `forwardPorts`, `mounts`, custom
  `workspaceMount` strings — ignored.

## Lifecycle

keel owns the container's lifecycle: there's no separate "up" step
to run.

1. **First use** (TUI `n` press, recipe run, `keel shell`):
   - For `build` mode, build the image and tag it
     `keel-devcontainer-<project>:<sha>`, where `<sha>` is a hash
     of the dockerfile + build args. Skipped if the tag already
     exists.
   - `docker run -d` the container with `--name
     keel-devcontainer-<project>[-<worktree>]`, the workspace
     bind-mounted to `workspaceFolder`, and the devcontainer's
     `containerEnv` + `remoteUser` applied. Keep-alive command: a
     portable infinite `sleep` loop.
2. **Subsequent uses**: `docker exec -it <name> ...`. If the
   container is stopped, keel `docker start`s it first.

To rebuild after editing the Dockerfile or build args, change a
build input — the hash changes, keel detects the missing tag, and
rebuilds. To rebuild without changing inputs:

```sh
docker rm -f keel-devcontainer-<project>
docker image rm keel-devcontainer-<project>:<tag>
```

## Worktree isolation

Each git worktree gets its own devcontainer.

- Container name: `keel-devcontainer-<project-slug>-<worktree-slug>`
- Bind mount: the worktree's own root → `workspaceFolder`
- Labels: `keel.devcontainer.root=<absolute path>`,
  `keel.devcontainer.worktree=<slug>`

This mirrors `WorktreesConfig::isolate_compose` for the compose
backend. Two worktrees of the same project can be running
side-by-side, each with an independent container.

## Doctor

`keel doctor` reports the active devcontainer setup (config path,
dockerfile presence, container name → image → workspace folder) and
warns on privilege-escalating `runArgs`. The check is skipped
entirely when `[devcontainer] enabled = false`.

## See also

- [Container Backends](Container-Backends) for the existing
  compose / podman / docker routing — devcontainer is orthogonal.
- [Worktrees](Worktrees) for slug derivation and isolation.
