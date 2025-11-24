use crate::core::graph::Graph;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphChecksum(pub u64);

/// Compute deterministic checksum of graph structure
pub fn compute_checksum(graph: &Graph) -> GraphChecksum {
    let mut hasher = DefaultHasher::new();

    // Hash all nodes (sorted by ID for determinism)
    let mut nodes: Vec<_> = graph.petgraph().node_indices().collect();
    nodes.sort_by_key(|&idx| &graph.petgraph()[idx].id);

    for node_idx in nodes {
        let node = &graph.petgraph()[node_idx];
        node.id.hash(&mut hasher);
        node.processor_type.hash(&mut hasher);
        node.config_checksum.hash(&mut hasher);
    }

    // Hash all edges (sorted by connection ID for determinism)
    let mut edges: Vec<_> = graph.petgraph().edge_indices().collect();
    edges.sort_by_key(|&idx| &graph.petgraph()[idx].id);

    for edge_idx in edges {
        let edge = &graph.petgraph()[edge_idx];
        edge.id.hash(&mut hasher);
        edge.from_port.hash(&mut hasher);
        edge.to_port.hash(&mut hasher);
        // Don't hash port_type - structural only
    }

    GraphChecksum(hasher.finish())
}

/// Compute simple config checksum using Debug formatting
///
/// Phase 0: Simple implementation using Debug trait
/// TODO(Phase N): Handle config changes at runtime
///   - Option 1: Make configs immutable (remove processor + re-add to change config)
///   - Option 2: Add config change callbacks that mark graph dirty
///   - Option 3: Track config version in ProcessorConfig trait
///
/// TODO(Phase N): Handle Python processor implementation changes
///   - Option 1: Include impl_version field in config (manual)
///   - Option 2: Hash source code (fragile, Python-only)
///   - Option 3: Don't cache Python processors (conservative)
///   - Option 4: Time-based cache invalidation
pub fn compute_config_checksum<T: std::fmt::Debug>(config: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Simple approach: hash the Debug representation
    // Works for any config with Debug trait
    format!("{:?}", config).hash(&mut hasher);
    hasher.finish()
}
