// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut,
};
use crate::core::{LinkUniqueId, ProcessorUniqueId};

use super::super::traversal_source::link_at;

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
            .filter_map(|at| link_at(self.graph, self.links_from_another_runtime, at))
            .map(|link| link.id.clone())
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
            .filter_map(|at| link_at(self.graph, self.links_from_another_runtime, at))
            .map(|link| link.id.clone())
            .collect()
    }
}
