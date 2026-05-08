use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HookError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },

    #[error("invalid regex `{pattern}` for hook `{hook}`: {source}")]
    InvalidRegex {
        hook: String,
        pattern: String,
        #[source]
        source: regex::Error,
    },

    #[error("git invocation failed: {0}")]
    GitFailed(String),

    #[error(
        "{0} is not a git repository (run `git init` or `scaffl hooks install` from inside one)"
    )]
    NotARepo(PathBuf),

    #[error("refusing to overwrite non-scaffl hook at {path}")]
    HookExists { path: PathBuf },

    #[error("hook `{hook}` references entry that could not be parsed: {message}")]
    EntryParse { hook: String, message: String },

    #[error("hook `{hook}` requires a non-empty `entry`")]
    EntryMissing { hook: String },

    #[error(transparent)]
    Other(#[from] std::io::Error),
}
