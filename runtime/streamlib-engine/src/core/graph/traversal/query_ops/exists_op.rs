// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut};

impl<'a> NodeTraversal<'a> {
    /// Returns true if the traversal contains any nodes.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }
}

impl<'a> LinkTraversal<'a> {
    /// Returns true if the traversal contains any links.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }
}

impl<'a> NodeTraversalMut<'a> {
    /// Returns true if the traversal contains any nodes.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }
}

impl<'a> LinkTraversalMut<'a> {
    /// Returns true if the traversal contains any links.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }
}
