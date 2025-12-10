use crate::core::graph::{Link, LinkTraversal, ProcessorNode, ProcessorTraversal};

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
            ids: new_ids,
        }
    }
}

impl<'a> LinkTraversal<'a> {
    pub fn filter(self, predicate: impl Fn(&Link) -> bool) -> LinkTraversal<'a> {
        let new_ids = self
            .ids
            .iter()
            .filter_map(|&idx| self.graph.edge_weight(idx).map(|link| (idx, link)))
            .filter_map(|(idx, link)| predicate(link).then_some(idx))
            .collect();
        LinkTraversal {
            graph: self.graph,
            ids: new_ids,
        }
    }
}
