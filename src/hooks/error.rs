use crate::cache::CacheError;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HookError {
    #[error(transparent)]
    Cache(#[from] CacheError),

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
        "{0} is not a git repository (run `git init` or `croft hooks install` from inside one)"
    )]
    NotARepo(PathBuf),

    #[error("refusing to overwrite non-croft hook at {path}")]
    HookExists { path: PathBuf },

    #[error("hook `{hook}` references entry that could not be parsed: {message}")]
    EntryParse { hook: String, message: String },

    #[error("hook `{hook}` requires a non-empty `entry`")]
    EntryMissing { hook: String },

    #[error(
        "`repo: meta` references pre-commit's built-in hooks (check-hooks-apply, identity, …) \
         which croft does not implement. Remove the entry or replace it with an equivalent \
         `repo: local` hook."
    )]
    MetaRepoNotSupported,

    #[error(transparent)]
    Runtime(#[from] Box<crate::runtime::RuntimeError>),

    #[error(
        "hook `{hook}` references repo {repo} at rev `{rev}` but its `.pre-commit-hooks.yaml` \
         does not define an id matching `{hook}`."
    )]
    UpstreamHookMissing {
        repo: String,
        rev: String,
        hook: String,
    },

    #[error(transparent)]
    Other(#[from] std::io::Error),
}
