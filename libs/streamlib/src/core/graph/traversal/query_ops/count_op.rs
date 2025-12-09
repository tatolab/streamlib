// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkTraversal, ProcessorTraversal};

impl<'a> ProcessorTraversal<'a> {
    /// Get the count of vertices in the current traversal.
    pub fn count(self) -> ProcessorTraversal<'a> {
        ProcessorTraversal {
            graph: self.graph,
            ids: self.ids.into_iter().count(),
        }
    }
}

impl<'a> LinkTraversal<'a> {
    /// Get the count of edges  in the current traversal.
    pub fn count(self) -> LinkTraversal<'a> {
        LinkTraversal {
            graph: self.graph,
            ids: self.ids.into_iter().count(),
        }
    }
}
