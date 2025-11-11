use thiserror::Error;

#[derive(Error, Debug)]
pub enum AikvError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Wrong number of arguments for '{0}' command")]
    WrongArgCount(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Wrong type: {0}")]
    WrongType(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

pub type Result<T> = std::result::Result<T, AikvError>;
