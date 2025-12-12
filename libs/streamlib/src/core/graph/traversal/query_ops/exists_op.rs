// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut,
};

impl<'a> ProcessorTraversal<'a> {
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

impl<'a> ProcessorTraversalMut<'a> {
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
