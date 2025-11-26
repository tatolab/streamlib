//! Graph delta computation for incremental execution updates.

use std::collections::HashSet;

use crate::core::graph::ProcessorId;
use crate::core::link_channel::LinkId;

/// Represents the difference between desired state (Graph) and running state (ExecutionGraph).
#[derive(Debug, Default)]
pub struct GraphDelta {
    /// Processors in Graph but not yet spawned
    pub processors_to_add: Vec<ProcessorId>,
    /// Processors spawned but no longer in Graph
    pub processors_to_remove: Vec<ProcessorId>,
    /// Links in Graph but not yet wired
    pub links_to_add: Vec<LinkId>,
    /// Links wired but no longer in Graph
    pub links_to_remove: Vec<LinkId>,
    /// Processors with config changes (future use)
    pub processors_to_update: Vec<ProcessorConfigChange>,
    /// Links with config changes (future use)
    pub links_to_update: Vec<LinkConfigChange>,
}

impl GraphDelta {
    /// Check if there are no changes to apply.
    pub fn is_empty(&self) -> bool {
        self.processors_to_add.is_empty()
            && self.processors_to_remove.is_empty()
            && self.links_to_add.is_empty()
            && self.links_to_remove.is_empty()
            && self.processors_to_update.is_empty()
            && self.links_to_update.is_empty()
    }

    /// Total number of changes.
    pub fn change_count(&self) -> usize {
        self.processors_to_add.len()
            + self.processors_to_remove.len()
            + self.links_to_add.len()
            + self.links_to_remove.len()
            + self.processors_to_update.len()
            + self.links_to_update.len()
    }
}

/// Processor config change (for hot-reload, future use).
#[derive(Debug, Clone)]
pub struct ProcessorConfigChange {
    pub id: ProcessorId,
    pub old_config_checksum: u64,
    pub new_config_checksum: u64,
}

/// Link config change (capacity, buffer strategy, future use).
#[derive(Debug, Clone)]
pub struct LinkConfigChange {
    pub id: LinkId,
    pub new_capacity: Option<usize>,
}

/// Compute delta between desired (Graph) and running (ExecutionGraph) state.
pub fn compute_delta(
    graph_processor_ids: &HashSet<ProcessorId>,
    graph_link_ids: &HashSet<LinkId>,
    running_processor_ids: &HashSet<ProcessorId>,
    wired_link_ids: &HashSet<LinkId>,
) -> GraphDelta {
    let processors_to_add: Vec<_> = graph_processor_ids
        .difference(running_processor_ids)
        .cloned()
        .collect();

    let processors_to_remove: Vec<_> = running_processor_ids
        .difference(graph_processor_ids)
        .cloned()
        .collect();

    let links_to_add: Vec<_> = graph_link_ids.difference(wired_link_ids).cloned().collect();

    let links_to_remove: Vec<_> = wired_link_ids.difference(graph_link_ids).cloned().collect();

    GraphDelta {
        processors_to_add,
        processors_to_remove,
        links_to_add,
        links_to_remove,
        processors_to_update: Vec::new(),
        links_to_update: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_delta() {
        let delta = GraphDelta::default();
        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_delta_with_additions() {
        let graph_procs: HashSet<_> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let running_procs: HashSet<_> = ["a"].iter().map(|s| s.to_string()).collect();
        let graph_links: HashSet<_> = HashSet::new();
        let wired_links: HashSet<_> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert_eq!(delta.processors_to_add.len(), 2);
        assert!(delta.processors_to_add.contains(&"b".to_string()));
        assert!(delta.processors_to_add.contains(&"c".to_string()));
        assert!(delta.processors_to_remove.is_empty());
    }

    #[test]
    fn test_delta_with_removals() {
        let graph_procs: HashSet<_> = ["a"].iter().map(|s| s.to_string()).collect();
        let running_procs: HashSet<_> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let graph_links: HashSet<_> = HashSet::new();
        let wired_links: HashSet<_> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(delta.processors_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 2);
        assert!(delta.processors_to_remove.contains(&"b".to_string()));
        assert!(delta.processors_to_remove.contains(&"c".to_string()));
    }

    #[test]
    fn test_delta_with_link_changes() {
        use crate::core::link_channel::link_id::__private::new_unchecked;

        let graph_procs: HashSet<_> = HashSet::new();
        let running_procs: HashSet<_> = HashSet::new();

        let link1 = new_unchecked("link_1".to_string());
        let link2 = new_unchecked("link_2".to_string());
        let link3 = new_unchecked("link_3".to_string());

        let graph_links: HashSet<_> = [link1.clone(), link2.clone()].into_iter().collect();
        let wired_links: HashSet<_> = [link2.clone(), link3.clone()].into_iter().collect();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert_eq!(delta.links_to_add.len(), 1);
        assert!(delta.links_to_add.contains(&link1));
        assert_eq!(delta.links_to_remove.len(), 1);
        assert!(delta.links_to_remove.contains(&link3));
    }

    #[test]
    fn test_delta_no_changes() {
        let procs: HashSet<_> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let links: HashSet<_> = HashSet::new();

        let delta = compute_delta(&procs, &links, &procs, &links);

        assert!(delta.is_empty());
    }
}
