// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::{LinkTraversalMut, graph::NodeTraversalMut};

impl<'a> NodeTraversalMut<'a> {
    pub fn drop(self) -> NodeTraversalMut<'a> {
        let NodeTraversalMut { graph, ids } = self;

        let new_ids = ids
            .into_iter()
            .filter(|&id| graph.remove_node(id).is_none())
            .collect();

        NodeTraversalMut {
            graph,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversalMut<'a> {
    pub fn drop(self) -> LinkTraversalMut<'a> {
        let LinkTraversalMut { graph, ids } = self;

        let new_ids = ids
            .into_iter()
            .filter(|&id| graph.remove_edge(id).is_none())
            .collect();

        LinkTraversalMut {
            graph,
            ids: new_ids,
        }
    }
}
