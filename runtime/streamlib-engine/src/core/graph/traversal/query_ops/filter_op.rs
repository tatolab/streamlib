// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    Link, LinkTraversal, LinkTraversalMut, ProcessorNode, ProcessorTraversal, ProcessorTraversalMut,
};

use super::super::traversal_source::link_at;

impl<'a> ProcessorTraversal<'a> {
    pub fn filter(self, predicate: impl Fn(&ProcessorNode) -> bool) -> ProcessorTraversal<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter_map(|&idx| self.graph.node_weight(idx).map(|node| (idx, node)))
            .filter_map(|(idx, node)| predicate(node).then_some(idx))
            .collect();
        ProcessorTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn filter(self, predicate: impl Fn(&Link) -> bool) -> LinkTraversal<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter(|at| {
                link_at(self.graph, self.links_from_another_runtime, at).is_some_and(&predicate)
            })
            .cloned()
            .collect();
        LinkTraversal {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: new_ids,
        }
    }
}

impl<'a> ProcessorTraversalMut<'a> {
    pub fn filter(self, predicate: impl Fn(&ProcessorNode) -> bool) -> ProcessorTraversalMut<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter_map(|&idx| self.graph.node_weight(idx).map(|node| (idx, node)))
            .filter_map(|(idx, node)| predicate(node).then_some(idx))
            .collect();
        ProcessorTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn filter(self, predicate: impl Fn(&Link) -> bool) -> LinkTraversalMut<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter(|at| {
                link_at(self.graph, self.links_from_another_runtime, at).is_some_and(&predicate)
            })
            .cloned()
            .collect();
        LinkTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: new_ids,
        }
    }
}
