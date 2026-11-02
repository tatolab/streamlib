// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use thiserror::Error;

/// Errors that can occur during processor schema operations.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// Invalid schema name format.
    #[error("invalid schema name '{name}': {reason}")]
    InvalidName { name: String, reason: String },
}

/// Result type alias for schema operations.
pub type SchemaResult<T> = std::result::Result<T, SchemaError>;
