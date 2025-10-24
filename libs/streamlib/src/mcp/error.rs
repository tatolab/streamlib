//! Error types for streamlib-mcp

use thiserror::Error;

/// Result type for MCP operations
pub type Result<T> = std::result::Result<T, McpError>;

/// MCP server errors
#[derive(Error, Debug)]
pub enum McpError {
    /// Feature not yet implemented
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Processor registry error
    #[error("Registry error: {0}")]
    Registry(#[from] crate::core::StreamError),

    /// Resource not found
    #[error("Resource not found: {0}")]
    ResourceNotFound(String),

    /// Tool not found
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Invalid tool arguments
    #[error("Invalid arguments for tool {tool}: {message}")]
    InvalidArguments { tool: String, message: String },

    /// MCP protocol error
    #[error("MCP protocol error: {0}")]
    Protocol(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Configuration(String),
}
