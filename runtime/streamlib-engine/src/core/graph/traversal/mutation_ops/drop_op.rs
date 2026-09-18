// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{LinkTraversalMut, graph::ProcessorTraversalMut};

use super::super::traversal_source::LinkLocation;

impl<'a> ProcessorTraversalMut<'a> {
    pub fn drop(self) -> ProcessorTraversalMut<'a> {
        let ProcessorTraversalMut {
            graph,
            links_from_another_runtime,
            ids,
        } = self;

        let new_ids = ids
            .into_iter()
            .filter(|&id| {
                // petgraph cascades a node's incident edges; the links from
                // another runtime carry no edge, so a removed destination's
                // are forgotten here or they would outlive their processor.
                if let Some(removed) = graph.node_weight(id) {
                    links_from_another_runtime.forget_every_link_into(&removed.id);
                }
                graph.remove_node(id).is_none()
            })
            .collect();

        ProcessorTraversalMut {
            graph,
            links_from_another_runtime,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn drop(self) -> LinkTraversalMut<'a> {
        let LinkTraversalMut {
            graph,
            links_from_another_runtime,
            ids,
        } = self;

        let new_ids = ids
            .into_iter()
            .filter(|at| match at {
                LinkLocation::OnAnEdgeOfTheDigraph(edge) => graph.remove_edge(*edge).is_none(),
                LinkLocation::AmongTheLinksFromAnotherRuntime(link_id) => {
                    !links_from_another_runtime.forget(link_id)
                }
            })
            .collect();

        LinkTraversalMut {
            graph,
            links_from_another_runtime,
            ids: new_ids,
        }
    }
}
