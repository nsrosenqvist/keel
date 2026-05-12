//! `devcontainer.json` parser.
//!
//! Reads the v1-supported subset of the Development Containers
//! specification. The file format is JSONC (JSON with `//` comments
//! and trailing commas), so we parse via the `json5` crate.
//!
//! Auto-detect order matches the spec: `.devcontainer/devcontainer.json`
//! wins over `.devcontainer.json` when both exist.
//!
//! Unsupported fields that imply structurally different runtimes
//! (`dockerComposeFile`) produce a clear error pointing the user at the
//! existing compose backend; unsupported-but-harmless fields are
//! silently ignored.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DevcontainerConfigError {
    #[error("devcontainer.json not found (tried: {0})")]
    NotFound(String),

    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: json5::Error,
    },

    #[error(
        "{path}: `dockerComposeFile` devcontainers are not supported in ampelos v1. \
         Use `runtime.backend = \"compose\"` in ampelos.toml and route recipes \
         to the dev service with `in = \"<service>\"` instead."
    )]
    DockerComposeUnsupported { path: PathBuf },

    #[error("{path}: neither `image` nor `build.dockerfile` is set — nothing to run")]
    NoBuildSource { path: PathBuf },
}

/// The parsed devcontainer descriptor, normalised into ampelos's
/// internal shape. Field names track the spec; types are tightened
/// where ambiguity would force later code into a stringly-typed mess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevcontainerSpec {
    /// Absolute path the spec was loaded from. Used for error
    /// messages and for resolving relative paths inside the spec
    /// (`build.dockerfile`, `build.context`).
    pub source_path: PathBuf,

    /// Optional human-readable name. Not used by ampelos for
    /// identity (that's container-name derivation in `backend.rs`).
    pub name: Option<String>,

    /// What to run: a pre-built image, or a Dockerfile to build.
    pub source: ContainerSource,

    /// Workspace folder inside the container. The spec default is
    /// `/workspaces/<repo-name>`; that defaulting happens at
    /// `DevcontainerBackend` construction time because it needs the
    /// project root.
    pub workspace_folder: Option<String>,

    /// Extra args passed to `docker run` when starting the container.
    /// Common entries: `--init`, `--cap-add=...`. v1 passes these
    /// through; `backend.rs` logs a warning for entries that elevate
    /// privileges (`--privileged`, `--cap-add`, `--network=host`).
    pub run_args: Vec<String>,

    /// Environment baked into the container at `docker run` time.
    /// Survives across `exec` calls but isn't merged on top of
    /// per-exec env — that's `remote_env`'s job.
    pub container_env: BTreeMap<String, String>,

    /// Environment injected on every `docker exec`. Merged on top of
    /// ampelos's recipe env (recipe env wins on conflict).
    pub remote_env: BTreeMap<String, String>,

    /// User to exec as inside the container. `None` means "let the
    /// image's default user win" (typically `root` unless the image
    /// sets `USER`).
    pub remote_user: Option<String>,
}

/// Either a pre-built image reference or a build directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerSource {
    /// `image: "..."` — pulled (if needed) and run as-is.
    Image(String),

    /// `build: { dockerfile, context, args }` — built locally and
    /// tagged deterministically by content hash.
    Build {
        /// Path to the Dockerfile, resolved relative to the
        /// devcontainer.json's directory.
        dockerfile: PathBuf,
        /// Build context directory, resolved relative to the
        /// devcontainer.json's directory. Defaults to the devcontainer
        /// directory itself per the spec.
        context: PathBuf,
        /// `--build-arg KEY=VALUE` pairs.
        args: BTreeMap<String, String>,
    },
}

/// Raw deserialisation target. Mirrors the spec field names so the
/// json5 layer doesn't need to know about ampelos conventions.
/// Unknown fields are tolerated (the spec is large and we only
/// support a slice of it).
#[derive(Debug, Default, Deserialize)]
struct RawSpec {
    #[serde(default)]
    name: Option<String>,

    #[serde(default)]
    image: Option<String>,

    #[serde(default)]
    build: Option<RawBuild>,

    #[serde(default, rename = "dockerComposeFile")]
    docker_compose_file: Option<serde_json::Value>,

    #[serde(default, rename = "workspaceFolder")]
    workspace_folder: Option<String>,

    #[serde(default, rename = "runArgs")]
    run_args: Vec<String>,

    #[serde(default, rename = "containerEnv")]
    container_env: BTreeMap<String, String>,

    #[serde(default, rename = "remoteEnv")]
    remote_env: BTreeMap<String, String>,

    #[serde(default, rename = "remoteUser")]
    remote_user: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBuild {
    #[serde(default)]
    dockerfile: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    args: BTreeMap<String, String>,
}

impl DevcontainerSpec {
    /// Locate a devcontainer.json under `project_root`, honouring the
    /// user's `[devcontainer].path` override if provided.
    pub fn discover(
        project_root: &Path,
        override_path: Option<&str>,
    ) -> Result<PathBuf, DevcontainerConfigError> {
        if let Some(p) = override_path {
            let candidate = if Path::new(p).is_absolute() {
                PathBuf::from(p)
            } else {
                project_root.join(p)
            };
            if candidate.is_file() {
                return Ok(candidate);
            }
            return Err(DevcontainerConfigError::NotFound(
                candidate.display().to_string(),
            ));
        }
        let tried = [".devcontainer/devcontainer.json", ".devcontainer.json"];
        for rel in tried {
            let candidate = project_root.join(rel);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(DevcontainerConfigError::NotFound(tried.join(", ")))
    }

    /// Parse the file at `path`. The path is stored on the returned
    /// spec for downstream error messages and relative-path resolution.
    pub fn load(path: &Path) -> Result<Self, DevcontainerConfigError> {
        let raw_text =
            std::fs::read_to_string(path).map_err(|source| DevcontainerConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        Self::from_str(path, &raw_text)
    }

    /// Parse pre-loaded JSONC text. Split out for testability — the
    /// parser tests round-trip strings without hitting the filesystem.
    pub fn from_str(path: &Path, text: &str) -> Result<Self, DevcontainerConfigError> {
        let raw: RawSpec =
            json5::from_str(text).map_err(|source| DevcontainerConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?;

        if raw.docker_compose_file.is_some() {
            return Err(DevcontainerConfigError::DockerComposeUnsupported {
                path: path.to_path_buf(),
            });
        }

        let parent = path.parent().unwrap_or(Path::new(""));
        let source = match (raw.image, raw.build) {
            (Some(image), _) => ContainerSource::Image(image),
            (None, Some(build)) => {
                let dockerfile_rel = build.dockerfile.as_deref().unwrap_or("Dockerfile");
                let dockerfile = parent.join(dockerfile_rel);
                let context = build
                    .context
                    .as_deref()
                    .map(|c| parent.join(c))
                    .unwrap_or_else(|| parent.to_path_buf());
                ContainerSource::Build {
                    dockerfile,
                    context,
                    args: build.args,
                }
            }
            (None, None) => {
                return Err(DevcontainerConfigError::NoBuildSource {
                    path: path.to_path_buf(),
                });
            }
        };

        Ok(Self {
            source_path: path.to_path_buf(),
            name: raw.name,
            source,
            workspace_folder: raw.workspace_folder,
            run_args: raw.run_args,
            container_env: raw.container_env,
            remote_env: raw.remote_env,
            remote_user: raw.remote_user,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn path(name: &str) -> PathBuf {
        PathBuf::from("/proj").join(name)
    }

    #[test]
    fn parses_image_only() {
        let src = r#"{ "image": "mcr.microsoft.com/devcontainers/base:ubuntu" }"#;
        let spec = DevcontainerSpec::from_str(&path("devcontainer.json"), src).unwrap();
        assert_eq!(
            spec.source,
            ContainerSource::Image("mcr.microsoft.com/devcontainers/base:ubuntu".into())
        );
        assert!(spec.workspace_folder.is_none());
        assert!(spec.container_env.is_empty());
    }

    #[test]
    fn parses_with_line_and_block_comments() {
        let src = r#"{
            // this image is the rust dev image
            "image": "rust:1.88", /* trailing comment */
            "name": "rust-dev",
        }"#;
        let spec = DevcontainerSpec::from_str(&path("devcontainer.json"), src).unwrap();
        assert_eq!(spec.name.as_deref(), Some("rust-dev"));
        assert_eq!(spec.source, ContainerSource::Image("rust:1.88".into()));
    }

    #[test]
    fn parses_build_with_defaults() {
        let src = r#"{
            "build": {
                "dockerfile": "Dockerfile",
            }
        }"#;
        let spec =
            DevcontainerSpec::from_str(&path(".devcontainer/devcontainer.json"), src).unwrap();
        match spec.source {
            ContainerSource::Build {
                dockerfile,
                context,
                args,
            } => {
                assert_eq!(dockerfile, PathBuf::from("/proj/.devcontainer/Dockerfile"));
                // Spec default context: the devcontainer.json's directory.
                assert_eq!(context, PathBuf::from("/proj/.devcontainer"));
                assert!(args.is_empty());
            }
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn parses_build_with_explicit_context_and_args() {
        let src = r#"{
            "build": {
                "dockerfile": "build/Dockerfile",
                "context": "..",
                "args": { "RUST_VERSION": "1.88" }
            }
        }"#;
        let spec =
            DevcontainerSpec::from_str(&path(".devcontainer/devcontainer.json"), src).unwrap();
        match spec.source {
            ContainerSource::Build {
                dockerfile,
                context,
                args,
            } => {
                assert_eq!(
                    dockerfile,
                    PathBuf::from("/proj/.devcontainer/build/Dockerfile")
                );
                assert_eq!(context, PathBuf::from("/proj/.devcontainer/.."));
                assert_eq!(args.get("RUST_VERSION").map(String::as_str), Some("1.88"));
            }
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn parses_full_spec_subset() {
        let src = r#"{
            "name": "test",
            "image": "alpine:latest",
            "workspaceFolder": "/workspaces/test",
            "runArgs": ["--init", "--cap-add=SYS_PTRACE"],
            "containerEnv": { "TZ": "UTC" },
            "remoteEnv": { "EDITOR": "vim" },
            "remoteUser": "vscode",
        }"#;
        let spec = DevcontainerSpec::from_str(&path("devcontainer.json"), src).unwrap();
        assert_eq!(spec.workspace_folder.as_deref(), Some("/workspaces/test"));
        assert_eq!(spec.run_args, vec!["--init", "--cap-add=SYS_PTRACE"]);
        assert_eq!(
            spec.container_env.get("TZ").map(String::as_str),
            Some("UTC")
        );
        assert_eq!(
            spec.remote_env.get("EDITOR").map(String::as_str),
            Some("vim")
        );
        assert_eq!(spec.remote_user.as_deref(), Some("vscode"));
    }

    #[test]
    fn rejects_docker_compose_file() {
        let src = r#"{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "workspaceFolder": "/workspace"
        }"#;
        let err =
            DevcontainerSpec::from_str(&path("devcontainer.json"), src).expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("dockerComposeFile"), "got: {msg}");
        assert!(msg.contains("runtime.backend"), "got: {msg}");
    }

    #[test]
    fn rejects_when_neither_image_nor_build_set() {
        let src = r#"{ "name": "empty" }"#;
        let err =
            DevcontainerSpec::from_str(&path("devcontainer.json"), src).expect_err("must reject");
        assert!(err.to_string().contains("nothing to run"));
    }

    #[test]
    fn image_wins_when_both_image_and_build_provided() {
        // Spec ambiguity: if both are set, `image` wins (build is
        // typically left behind in template copy-pastes).
        let src = r#"{
            "image": "alpine:latest",
            "build": { "dockerfile": "Dockerfile" }
        }"#;
        let spec = DevcontainerSpec::from_str(&path("devcontainer.json"), src).unwrap();
        assert_eq!(spec.source, ContainerSource::Image("alpine:latest".into()));
    }

    #[test]
    fn ignores_unknown_fields() {
        // Spec is huge; we only support a subset. Unknowns must not
        // fail parsing or users couldn't share configs across tools.
        let src = r#"{
            "image": "alpine",
            "features": { "ghcr.io/devcontainers/features/git:1": {} },
            "postCreateCommand": "echo hi",
            "forwardPorts": [3000]
        }"#;
        let spec = DevcontainerSpec::from_str(&path("devcontainer.json"), src).unwrap();
        assert_eq!(spec.source, ContainerSource::Image("alpine".into()));
    }

    #[test]
    fn discover_prefers_dotdir_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".devcontainer")).unwrap();
        std::fs::write(root.join(".devcontainer/devcontainer.json"), "{}").unwrap();
        std::fs::write(root.join(".devcontainer.json"), "{}").unwrap();

        let found = DevcontainerSpec::discover(root, None).unwrap();
        assert_eq!(found, root.join(".devcontainer/devcontainer.json"));
    }

    #[test]
    fn discover_falls_back_to_dotfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".devcontainer.json"), "{}").unwrap();

        let found = DevcontainerSpec::discover(root, None).unwrap();
        assert_eq!(found, root.join(".devcontainer.json"));
    }

    #[test]
    fn discover_honours_override() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("custom")).unwrap();
        std::fs::write(root.join("custom/dev.json"), "{}").unwrap();

        let found = DevcontainerSpec::discover(root, Some("custom/dev.json")).unwrap();
        assert_eq!(found, root.join("custom/dev.json"));
    }

    #[test]
    fn discover_missing_override_errors_with_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = DevcontainerSpec::discover(tmp.path(), Some("nope.json")).expect_err("must fail");
        assert!(matches!(err, DevcontainerConfigError::NotFound(_)));
    }

    #[test]
    fn discover_no_config_errors_with_paths_tried() {
        let tmp = tempfile::tempdir().unwrap();
        let err = DevcontainerSpec::discover(tmp.path(), None).expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains(".devcontainer/devcontainer.json"));
        assert!(msg.contains(".devcontainer.json"));
    }
}
