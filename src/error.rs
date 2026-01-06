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

    #[error("Key not found")]
    KeyNotFound,

    #[error("Wrong type: {0}")]
    WrongType(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Persistence error: {0}")]
    Persistence(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Script error: {0}")]
    Script(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Invalid: {0}")]
    Invalid(String),

    #[error("MOVED {0} {1}")]
    Moved(u16, String),

    #[error("ASK {0} {1}")]
    Ask(u16, String),

    #[error("CROSSSLOT Keys in request don't hash to the same slot")]
    CrossSlot,

    #[error("Cluster support is not enabled")]
    ClusterDisabled,

    #[error("Unknown error: {0}")]
    Unknown(String),
}

pub type Result<T> = std::result::Result<T, AikvError>;
