// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// A component `graph` renders on the node or link that stores it, under a key of its own of
/// `Onto`, what `graph` renders of that node's or link's components.
pub trait GraphRenders<Onto> {
    /// Render this component under its key of `components`.
    fn render_onto(&self, components: &mut Onto);
}
