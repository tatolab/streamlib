// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dynamic tap points for runtime observation.

use std::sync::atomic::{AtomicU64, Ordering};

/// Unique identifier for a tap point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TapId(pub(crate) u64);

impl TapId {
    /// Generate a new unique tap ID.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for TapId {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry for managing active tap points.
pub struct TapRegistry {
    // For now, a simple placeholder
    // Will be expanded when we implement full tap functionality
}

impl TapRegistry {
    /// Create a new tap registry.
    pub fn new() -> Self {
        Self {}
    }

    /// Remove a tap point by ID.
    pub fn remove(&self, _id: TapId) {
        // Placeholder - will be implemented with full tap infrastructure
    }
}

impl Default for TapRegistry {
    fn default() -> Self {
        Self::new()
    }
}
