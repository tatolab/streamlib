// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use thiserror::Error;

/// Errors that can occur during processor schema operations.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// Failed to parse YAML schema definition.
    #[error("failed to parse schema YAML: {0}")]
    ParseError(#[from] serde_yaml::Error),

    /// Schema file not found.
    #[error("schema file not found: {path}")]
    FileNotFound { path: String },

    /// Failed to read schema file.
    #[error("failed to read schema file: {0}")]
    IoError(#[from] std::io::Error),

    /// Invalid schema name format.
    #[error("invalid schema name '{name}': {reason}")]
    InvalidName { name: String, reason: String },

    /// Missing required field in schema.
    #[error("missing required field '{field}' in schema")]
    MissingField { field: String },
}

/// Result type alias for schema operations.
pub type SchemaResult<T> = std::result::Result<T, SchemaError>;
