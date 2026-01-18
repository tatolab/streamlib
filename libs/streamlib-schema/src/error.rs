// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Error types for schema operations.

use thiserror::Error;

/// Errors that can occur during schema operations.
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

    /// Invalid field type.
    #[error("invalid field type '{type_name}' in field '{field_name}'")]
    InvalidFieldType {
        field_name: String,
        type_name: String,
    },

    /// Missing required field in schema.
    #[error("missing required field '{field}' in schema")]
    MissingField { field: String },

    /// Schema not found in registry.
    #[error("schema '{name}' not found in registry")]
    NotFound { name: String },

    /// Schema version mismatch.
    #[error("schema version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: String, found: String },

    /// Code generation error.
    #[error("code generation failed: {0}")]
    CodegenError(String),
}

/// Result type alias for schema operations.
pub type Result<T> = std::result::Result<T, SchemaError>;
