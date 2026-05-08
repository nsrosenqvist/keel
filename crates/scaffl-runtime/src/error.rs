use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("no such recipe or command: {name}")]
    UnknownCommand {
        name: String,
        suggestion: Option<String>,
    },

    #[error("recipe `{recipe}` references unknown dependency `{dep}`")]
    UnknownDependency { recipe: String, dep: String },

    #[error("dependency cycle detected involving recipe `{0}`")]
    DependencyCycle(String),

    #[error("required environment variable `{0}` is not set and has no default")]
    RequiredEnvMissing(String),

    #[error(
        "env var `{var}` from_command `{command}` failed with exit code {}",
        .exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
    )]
    EnvCommandFailed {
        var: String,
        command: String,
        exit_code: Option<i32>,
    },

    #[error("failed to parse argv from `{input}`: {message}")]
    ArgvParse { input: String, message: String },

    #[error("failed to read .env file at {path}: {source}")]
    DotenvIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse .env file at {path}: {source}")]
    DotenvParse {
        path: PathBuf,
        #[source]
        source: dotenvy::Error,
    },

    #[error(transparent)]
    Backend(#[from] scaffl_container::BackendError),

    #[error(transparent)]
    Config(#[from] scaffl_config::ConfigError),
}
