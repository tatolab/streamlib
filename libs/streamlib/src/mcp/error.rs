
use thiserror::Error;

pub type Result<T> = std::result::Result<T, McpError>;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Registry error: {0}")]
    Registry(#[from] crate::core::StreamError),

    #[error("Resource not found: {0}")]
    ResourceNotFound(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Invalid arguments for tool {tool}: {message}")]
    InvalidArguments { tool: String, message: String },

    #[error("MCP protocol error: {0}")]
    Protocol(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Runtime error: {0}")]
    Runtime(String),
}
