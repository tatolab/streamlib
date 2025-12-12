// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut};
use crate::core::{LinkUniqueId, ProcessorUniqueId};

impl<'a> ProcessorTraversal<'a> {
    pub fn ids(self) -> Vec<ProcessorUniqueId> {
        self.ids
            .iter()
            .filter_map(|&node_index| {
                self.graph
                    .node_weight(node_index)
                    .map(|node| node.id.clone())
            })
            .collect()
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn ids(self) -> Vec<LinkUniqueId> {
        self.ids
            .iter()
            .filter_map(|&edge_index| {
                self.graph
                    .edge_weight(edge_index)
                    .map(|link| link.id.clone())
            })
            .collect()
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    pub fn ids(self) -> Vec<ProcessorUniqueId> {
        self.ids
            .iter()
            .filter_map(|&node_index| {
                self.graph
                    .node_weight(node_index)
                    .map(|node| node.id.clone())
            })
            .collect()
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn ids(self) -> Vec<LinkUniqueId> {
        self.ids
            .iter()
            .filter_map(|&edge_index| {
                self.graph
                    .edge_weight(edge_index)
                    .map(|link| link.id.clone())
            })
            .collect()
    }
}
