// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::json_schema::{GraphLinkComponentsOnTheWire, GraphNodeComponentsOnTheWire};

/// A component `graph` renders on the node that stores it, under a key of its own.
pub trait NodeComponentGraphRenders {
    /// Render this component under its key of `components`.
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire);
}

/// A component `graph` renders on the link that stores it, under a key of its own.
pub trait LinkComponentGraphRenders {
    /// Render this component under its key of `components`.
    fn render_onto(&self, components: &mut GraphLinkComponentsOnTheWire);
}
