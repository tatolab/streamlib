// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::{LinkTraversalMut, graph::NodeTraversalMut};

impl<'a> NodeTraversalMut<'a> {
    pub fn drop(self) -> NodeTraversalMut<'a> {
        let NodeTraversalMut { graph, ids } = self;

        let mut neighbors = Vec::new();
        let new_ids = ids
            .into_iter()
            .filter(|&id| {
                neighbors.extend(
                    graph
                        .neighbors_undirected(id)
                        .map(|neighbor| graph[neighbor].id.clone()),
                );
                graph.remove_node(id).is_none()
            })
            .collect();
        // Taking a node out moves the index of the graph's last node, so each neighbor is found
        // again by its id.
        for neighbor in neighbors {
            if let Some(node_index) = graph
                .node_indices()
                .find(|node_index| graph[*node_index].id == neighbor)
            {
                crate::core::graph::reshape_the_pin_group_slots_of(graph, node_index);
            }
        }

        NodeTraversalMut {
            graph,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn drop(self) -> LinkTraversalMut<'a> {
        let LinkTraversalMut { graph, ids } = self;

        let mut ends = Vec::new();
        let new_ids = ids
            .into_iter()
            .filter(|&id| {
                ends.extend(graph.edge_endpoints(id).map(|(from, to)| [from, to]));
                graph.remove_edge(id).is_none()
            })
            .collect();
        for node_index in ends.into_iter().flatten() {
            crate::core::graph::reshape_the_pin_group_slots_of(graph, node_index);
        }

        LinkTraversalMut {
            graph,
            ids: new_ids,
        }
    }
}
