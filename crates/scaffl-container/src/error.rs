use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("backend binary not found: {0}")]
    BinaryNotFound(String),

    #[error("service `{service}` is {status}")]
    ServiceUnavailable { service: String, status: String },

    #[error("backend invocation failed: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("backend produced non-utf8 output")]
    InvalidUtf8(#[from] std::str::Utf8Error),

    #[error("backend reported error: {0}")]
    Reported(String),
}
