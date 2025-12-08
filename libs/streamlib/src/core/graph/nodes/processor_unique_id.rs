// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

/// Unique identifier for a processor node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessorUniqueId(String);

impl ProcessorUniqueId {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the inner string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProcessorUniqueId {
    fn default() -> Self {
        Self(format!("P{}", cuid2::create_id()))
    }
}

impl Deref for ProcessorUniqueId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for ProcessorUniqueId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ProcessorUniqueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProcessorUniqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ProcessorUniqueId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ProcessorUniqueId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<ProcessorUniqueId> for String {
    fn from(id: ProcessorUniqueId) -> Self {
        id.0
    }
}

impl From<&ProcessorUniqueId> for String {
    fn from(id: &ProcessorUniqueId) -> Self {
        id.0.clone()
    }
}

impl From<&ProcessorUniqueId> for ProcessorUniqueId {
    fn from(id: &ProcessorUniqueId) -> Self {
        Self(id.0.clone())
    }
}

impl PartialEq<str> for ProcessorUniqueId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<ProcessorUniqueId> for str {
    fn eq(&self, other: &ProcessorUniqueId) -> bool {
        self == other.0
    }
}

impl PartialEq<&str> for ProcessorUniqueId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ProcessorUniqueId> for &str {
    fn eq(&self, other: &ProcessorUniqueId) -> bool {
        *self == other.0
    }
}

impl PartialEq<String> for ProcessorUniqueId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ProcessorUniqueId> for String {
    fn eq(&self, other: &ProcessorUniqueId) -> bool {
        *self == other.0
    }
}
