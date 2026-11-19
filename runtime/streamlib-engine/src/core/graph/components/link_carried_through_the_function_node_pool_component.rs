// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::LinkUniqueId;

/// Marks one of the two links a link from a function node island into a
/// reader that cannot evaluate it is carried as, through the stream's function
/// node pool; `graph` renders the pair once, as the carried link, so the mark itself is never
/// rendered.
pub struct LinkCarriedThroughTheFunctionNodePoolComponent {
    /// The link as the graph was given it.
    pub carried_link_id: LinkUniqueId,
}
