//! Error types for streamlib
//!
//! Defines the core error types used throughout streamlib.
//! Platform-specific crates can extend these with their own error types.

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

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result type that uses StreamError
pub type Result<T> = std::result::Result<T, StreamError>;
