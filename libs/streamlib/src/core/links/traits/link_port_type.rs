//! Type of data that flows through a link port.

use serde::{Deserialize, Serialize};

/// Type of data that flows through a link port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPortType {
    Video,
    Audio,
    Data,
}

impl LinkPortType {
    /// Default ring buffer capacity for this port type.
    pub fn default_capacity(&self) -> usize {
        match self {
            LinkPortType::Video => 3,
            LinkPortType::Audio => 32,
            LinkPortType::Data => 16,
        }
    }

    /// Check if this port type is compatible with another.
    pub fn compatible_with(&self, other: &LinkPortType) -> bool {
        self == other
    }
}
