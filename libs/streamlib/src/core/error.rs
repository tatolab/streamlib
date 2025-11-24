use thiserror::Error;

#[derive(Error, Debug)]
pub enum StreamError {
    #[error("GPU operation failed: {0}")]
    GpuError(String),

    #[error("Shader compilation failed: {0}")]
    ShaderCompilation(String),

    #[error("Texture operation failed: {0}")]
    TextureError(String),

    #[error("Stream graph error: {0}")]
    GraphError(String),

    #[error("Port connection error: {0}")]
    PortError(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Connection already exists: {0}")]
    ConnectionAlreadyExists(String),

    #[error("Connection not found: {0}")]
    ConnectionNotFound(String),

    #[error("Buffer operation failed: {0}")]
    BufferError(String),

    #[error("Clock synchronization error: {0}")]
    ClockError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, StreamError>;
