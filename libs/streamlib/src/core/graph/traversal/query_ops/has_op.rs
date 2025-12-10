use crate::core::graph::{Component, GraphEdge, GraphNode, LinkTraversal, ProcessorTraversal};

impl<'a> ProcessorTraversal<'a> {
    pub fn has<C: Component>(self) -> ProcessorTraversal<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter_map(|&idx| self.graph.node_weight(idx).map(|node| (idx, node)))
            .filter_map(|(idx, node)| node.has::<C>().then_some(idx))
            .collect();

        ProcessorTraversal {
            graph: self.graph,
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn has<C: Component>(self) -> LinkTraversal<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter_map(|&idx| self.graph.edge_weight(idx).map(|link| (idx, link)))
            .filter_map(|(idx, link)| link.has::<C>().then_some(idx))
            .collect();

        LinkTraversal {
            graph: self.graph,
            ids: new_ids,
        }
    }
}
