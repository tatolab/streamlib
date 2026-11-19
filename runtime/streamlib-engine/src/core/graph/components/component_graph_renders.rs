// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// A component that `graph` renders under its own key of `Onto`, the components of the node or
/// link that stores it.
pub trait GraphRenders<Onto> {
    /// Render this component under its key of `components`.
    fn render_onto(&self, components: &mut Onto);
}
