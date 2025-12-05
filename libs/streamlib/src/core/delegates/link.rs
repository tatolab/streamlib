// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link delegate trait for wiring lifecycle callbacks.

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::graph::Link;
use crate::core::links::LinkId;

/// Delegate for link wiring lifecycle events.
///
/// Provides hooks for observing and customizing link wiring:
/// - Wiring: will_wire, did_wire
/// - Unwiring: will_unwire, did_unwire
///
/// All methods have default no-op implementations, so you only need
/// to override the ones you care about.
///
/// A blanket implementation is provided for `Arc<dyn LinkDelegate>`,
/// so you can pass an Arc directly where a `LinkDelegate` is expected.
pub trait LinkDelegate: Send + Sync {
    /// Called before a link is wired (ring buffer created).
    fn will_wire(&self, _link: &Link) -> Result<()> {
        Ok(())
    }

    /// Called after a link is successfully wired.
    fn did_wire(&self, _link: &Link) -> Result<()> {
        Ok(())
    }

    /// Called before a link is unwired (ring buffer destroyed).
    fn will_unwire(&self, _link_id: &LinkId) -> Result<()> {
        Ok(())
    }

    /// Called after a link is unwired.
    fn did_unwire(&self, _link_id: &LinkId) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// Blanket implementation for Arc wrapper
// =============================================================================

impl LinkDelegate for Arc<dyn LinkDelegate> {
    fn will_wire(&self, link: &Link) -> Result<()> {
        (**self).will_wire(link)
    }

    fn did_wire(&self, link: &Link) -> Result<()> {
        (**self).did_wire(link)
    }

    fn will_unwire(&self, link_id: &LinkId) -> Result<()> {
        (**self).will_unwire(link_id)
    }

    fn did_unwire(&self, link_id: &LinkId) -> Result<()> {
        (**self).did_unwire(link_id)
    }
}
