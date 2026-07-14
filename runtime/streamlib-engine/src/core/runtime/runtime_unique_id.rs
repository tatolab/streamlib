// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

/// Unique identifier for a runtime instance.
///
/// Generated automatically or loaded from `STREAMLIB_RUNTIME_ID` environment variable.
/// Use stable IDs in production for consistent cache paths across restarts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeUniqueId(String);

impl RuntimeUniqueId {
    /// Create a new runtime ID from environment variable or generate one.
    pub fn new() -> Self {
        Self::from_env_or_generate()
    }

    /// Load from STREAMLIB_RUNTIME_ID env var, or generate a new ID.
    pub fn from_env_or_generate() -> Self {
        if let Ok(id) = std::env::var("STREAMLIB_RUNTIME_ID") {
            tracing::info!("Using runtime ID from STREAMLIB_RUNTIME_ID: {}", id);
            return Self(id);
        }
        let id = format!("R{}", cuid2::create_id());
        tracing::trace!(
            "Generated runtime ID: {}. Set STREAMLIB_RUNTIME_ID env var for stable IDs in production.",
            id
        );
        Self(id)
    }

    /// Get the inner string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RuntimeUniqueId {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for RuntimeUniqueId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for RuntimeUniqueId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RuntimeUniqueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeUniqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RuntimeUniqueId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RuntimeUniqueId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<RuntimeUniqueId> for String {
    fn from(id: RuntimeUniqueId) -> Self {
        id.0
    }
}
