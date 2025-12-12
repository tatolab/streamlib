// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{de::DeserializeOwned, Serialize};

/// Trait for processor configuration types.
///
/// All processor configs must be pure data that can round-trip through JSON.
pub trait Config:
    Send + Sync + 'static + Default + Serialize + DeserializeOwned + PartialEq
{
    /// Validate that config can round-trip through JSON without data loss.
    fn validate_round_trip(&self) -> Result<(), ConfigValidationError> {
        let json = serde_json::to_value(self)
            .map_err(|e| ConfigValidationError::SerializationFailed(e.to_string()))?;
        let round_tripped: Self = serde_json::from_value(json)
            .map_err(|e| ConfigValidationError::DeserializationFailed(e.to_string()))?;
        if self != &round_tripped {
            return Err(ConfigValidationError::RoundTripMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValidationError {
    SerializationFailed(String),
    DeserializationFailed(String),
    RoundTripMismatch,
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SerializationFailed(e) => write!(f, "Config serialization failed: {}", e),
            Self::DeserializationFailed(e) => write!(f, "Config deserialization failed: {}", e),
            Self::RoundTripMismatch => write!(
                f,
                "Config round-trip mismatch: some fields may be skipped during serialization"
            ),
        }
    }
}

impl std::error::Error for ConfigValidationError {}

/// Blanket implementation for all types meeting the requirements.
impl<T> Config for T where
    T: Send + Sync + 'static + Default + Serialize + DeserializeOwned + PartialEq
{
}
