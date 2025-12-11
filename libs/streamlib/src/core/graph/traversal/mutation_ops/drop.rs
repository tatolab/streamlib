use crate::core::{graph::ProcessorTraversalMut, LinkTraversalMut};

impl<'a> ProcessorTraversalMut<'a> {
    pub fn drop(self) -> ProcessorTraversalMut<'a> {
        let ProcessorTraversalMut { graph, ids } = self;

        let new_ids = ids
            .into_iter()
            .filter(|&id| graph.remove_node(id).is_none())
            .collect();

        ProcessorTraversalMut {
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
