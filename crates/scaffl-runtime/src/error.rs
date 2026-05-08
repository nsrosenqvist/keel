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

    #[error(transparent)]
    Backend(#[from] scaffl_container::BackendError),

    #[error(transparent)]
    Config(#[from] scaffl_config::ConfigError),
}
