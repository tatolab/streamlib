// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut};
use crate::core::{LinkUniqueId, NodeRunId};

impl<'a> NodeTraversal<'a> {
    pub fn ids(self) -> Vec<NodeRunId> {
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

impl<'a> NodeTraversalMut<'a> {
    pub fn ids(self) -> Vec<NodeRunId> {
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
