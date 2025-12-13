// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Lock-free pause gate for processors.
///
/// Allows pausing individual processors without blocking. The gate is checked
/// at multiple points (thread runner, link writers/readers) to prevent
/// unnecessary processing when paused.
///
/// This is an ECS component attached to processor entities.
pub struct ProcessorPauseGateComponent(Arc<AtomicBool>);

impl ProcessorPauseGateComponent {
    /// Create a new pause gate (not paused by default).
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Returns true if the processor is currently paused.
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Returns true if processing should proceed (not paused).
    pub fn should_process(&self) -> bool {
        !self.is_paused()
    }

    /// Set the paused state.
    pub fn set_paused(&self, paused: bool) {
        self.0.store(paused, Ordering::Release);
    }

    /// Pause the processor.
    pub fn pause(&self) {
        self.set_paused(true);
    }

    /// Resume the processor.
    pub fn resume(&self) {
        self.set_paused(false);
    }

    /// Get a clone of the inner Arc for sharing with other threads.
    pub fn clone_inner(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

impl Default for ProcessorPauseGateComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ProcessorPauseGateComponent {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl JsonSerializableComponent for ProcessorPauseGateComponent {
    fn json_key(&self) -> &'static str {
        "paused"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(self.is_paused())
    }
}
