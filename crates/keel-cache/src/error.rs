use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("git invocation failed: {0}")]
    GitFailed(String),

    #[error(
        "failed to clone {repo} at rev `{rev}` into cache: {message}. \
         Make sure `git` is on PATH and the repo URL is reachable."
    )]
    CloneFailed {
        repo: String,
        rev: String,
        message: String,
    },

    #[error("repo `{repo}` is missing a `rev` — external repos must pin a revision")]
    MissingRev { repo: String },
}
