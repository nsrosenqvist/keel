use keel_cache::CacheError;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentsError {
    #[error(transparent)]
    Cache(#[from] CacheError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse agents manifest at {path}: {message}")]
    ManifestParse { path: PathBuf, message: String },

    #[error("invalid agents manifest at {path}: {message}")]
    ManifestInvalid { path: PathBuf, message: String },

    #[error(
        "agents source `{source_name}` would write `{dest}` but a non-managed file already \
         exists there. Rename the local file (e.g. `{suggested}`) and retry."
    )]
    LocalShadow {
        source_name: String,
        dest: PathBuf,
        suggested: String,
    },

    #[error("invalid agents config: {0}")]
    Config(String),
}
