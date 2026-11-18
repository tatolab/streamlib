// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::GraphRenders;
use crate::core::json_schema::{GraphLinkComponentsOnTheWire, GraphNodeComponentsOnTheWire};

/// Marks a node or a link a removal is deleting.
pub struct PendingDeletionComponent;

impl GraphRenders<GraphNodeComponentsOnTheWire> for PendingDeletionComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.pending_deletion = Some(true);
    }
}

impl GraphRenders<GraphLinkComponentsOnTheWire> for PendingDeletionComponent {
    fn render_onto(&self, components: &mut GraphLinkComponentsOnTheWire) {
        components.pending_deletion = Some(true);
    }
}
