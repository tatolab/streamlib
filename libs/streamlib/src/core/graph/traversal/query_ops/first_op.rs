// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkTraversal, ProcessorTraversal};

impl<'a> ProcessorTraversal<'a> {
    /// Get the first vertex in the current traversal.
    pub fn first(self) -> ProcessorTraversal<'a> {
        ProcessorTraversal {
            graph: self.graph,
            ids: self.ids.into_iter().take(1).collect(),
        }
    }
}

impl<'a> LinkTraversal<'a> {
    /// Get the first edge in the current traversal.
    pub fn first(self) -> LinkTraversal<'a> {
        LinkTraversal {
            graph: self.graph,
            ids: self.ids.into_iter().take(1).collect(),
        }
    }
}
