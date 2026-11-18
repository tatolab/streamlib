// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::NodeComponentGraphRenders;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;

pub struct NodePauseGateComponent(Arc<AtomicBool>);

impl NodePauseGateComponent {
    /// Create a new pause gate (not paused by default).
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

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

    pub fn pause(&self) {
        self.set_paused(true);
    }

    pub fn resume(&self) {
        self.set_paused(false);
    }

    /// Get a clone of the inner Arc for sharing with other threads.
    pub fn clone_inner(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

impl Default for NodePauseGateComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for NodePauseGateComponent {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl NodeComponentGraphRenders for NodePauseGateComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.paused = Some(self.is_paused());
    }
}
