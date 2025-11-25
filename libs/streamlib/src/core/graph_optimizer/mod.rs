//! Graph optimizer - analyzes graph and produces execution plans
//!
//! Phase 0: Only produces ExecutionPlan::Legacy (same as current behavior)

mod checksum;
mod execution_plan;

pub use checksum::{compute_config_checksum, GraphChecksum};
pub use execution_plan::ExecutionPlan;

use crate::core::error::Result;
use crate::core::graph::Graph;
use std::collections::HashMap;

/// Statistics about graph optimization
#[derive(Debug, Clone)]
pub struct GraphStats {
    /// Number of processors in graph
    pub processor_count: usize,
    /// Number of connections in graph
    pub connection_count: usize,
    /// Number of source processors (no incoming connections)
    pub source_count: usize,
    /// Number of sink processors (no outgoing connections)
    pub sink_count: usize,
    /// Graph checksum
    pub checksum: GraphChecksum,
    /// Whether execution plan was cached
    pub cache_hit: bool,
}

pub struct GraphOptimizer {
    /// Cache: graph checksum → execution plan
    plan_cache: HashMap<GraphChecksum, ExecutionPlan>,
}

impl GraphOptimizer {
    pub fn new() -> Self {
        Self {
            plan_cache: HashMap::new(),
        }
    }

    /// Analyze graph and produce execution plan
    ///
    /// Phase 0: Always returns ExecutionPlan::Legacy
    pub fn optimize(&mut self, graph: &Graph) -> Result<ExecutionPlan> {
        // Validate graph first
        graph.validate()?;

        // Compute checksum for cache lookup
        let checksum = checksum::compute_checksum(graph);

        // Check cache
        if let Some(cached_plan) = self.plan_cache.get(&checksum) {
            tracing::debug!(
                "✅ Using cached execution plan (checksum: {:x})",
                checksum.0
            );
            return Ok(cached_plan.clone());
        }

        // Cache miss - compute fresh plan
        tracing::debug!("🔍 Computing execution plan (checksum: {:x})", checksum.0);

        // Phase 0: Just return legacy plan (one thread per processor)
        let plan = self.compute_legacy_plan(graph);

        // Cache for future
        self.plan_cache.insert(checksum, plan.clone());

        Ok(plan)
    }

    /// Generate legacy execution plan (current behavior)
    fn compute_legacy_plan(&self, graph: &Graph) -> ExecutionPlan {
        // Get all processors and connections from graph
        let processors = graph.topological_order().unwrap_or_else(|_| vec![]);

        let connections: Vec<_> = graph
            .petgraph()
            .edge_indices()
            .map(|idx| graph.petgraph()[idx].id.clone())
            .collect();

        ExecutionPlan::Legacy {
            processors,
            connections,
        }
    }

    /// Get statistics about the last optimization
    pub fn stats(&self, graph: &Graph) -> GraphStats {
        let checksum = checksum::compute_checksum(graph);
        let cache_hit = self.plan_cache.contains_key(&checksum);

        GraphStats {
            processor_count: graph.petgraph().node_count(),
            connection_count: graph.petgraph().edge_count(),
            source_count: graph.find_sources().len(),
            sink_count: graph.find_sinks().len(),
            checksum,
            cache_hit,
        }
    }

    /// Clear cache (useful for testing or forcing recomputation)
    pub fn clear_cache(&mut self) {
        self.plan_cache.clear();
    }

    /// Get cache size
    pub fn cache_size(&self) -> usize {
        self.plan_cache.len()
    }
}

impl Default for GraphOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::connection_id::__private::new_unchecked;
    use crate::core::bus::PortType;

    fn create_linear_graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("transform".into(), "TransformProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        graph
            .add_connection_with_id(
                new_unchecked("conn_1"),
                "source.output".into(),
                "transform.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
            .add_connection_with_id(
                new_unchecked("conn_2"),
                "transform.output".into(),
                "sink.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
    }

    #[test]
    fn test_optimize_empty_graph() {
        let graph = Graph::new();
        let mut optimizer = GraphOptimizer::new();

        let plan = optimizer.optimize(&graph).unwrap();

        match plan {
            ExecutionPlan::Legacy {
                processors,
                connections,
            } => {
                assert!(processors.is_empty());
                assert!(connections.is_empty());
            }
        }
    }

    #[test]
    fn test_optimize_linear_graph() {
        let graph = create_linear_graph();
        let mut optimizer = GraphOptimizer::new();

        let plan = optimizer.optimize(&graph).unwrap();

        match plan {
            ExecutionPlan::Legacy {
                processors,
                connections,
            } => {
                assert_eq!(processors.len(), 3);
                assert_eq!(connections.len(), 2);

                // Verify topological order (source before transform before sink)
                let source_pos = processors.iter().position(|x| x == "source").unwrap();
                let transform_pos = processors.iter().position(|x| x == "transform").unwrap();
                let sink_pos = processors.iter().position(|x| x == "sink").unwrap();
                assert!(source_pos < transform_pos);
                assert!(transform_pos < sink_pos);
            }
        }
    }

    #[test]
    fn test_checksum_caching() {
        let graph = create_linear_graph();
        let mut optimizer = GraphOptimizer::new();

        // First optimization - cache miss
        let stats_before = optimizer.stats(&graph);
        assert!(!stats_before.cache_hit);
        assert_eq!(optimizer.cache_size(), 0);

        let _plan1 = optimizer.optimize(&graph).unwrap();
        assert_eq!(optimizer.cache_size(), 1);

        // Second optimization - cache hit
        let stats_after = optimizer.stats(&graph);
        assert!(stats_after.cache_hit);

        let _plan2 = optimizer.optimize(&graph).unwrap();
        assert_eq!(optimizer.cache_size(), 1); // Still 1, used cache
    }

    #[test]
    fn test_clear_cache() {
        let graph = create_linear_graph();
        let mut optimizer = GraphOptimizer::new();

        let _plan = optimizer.optimize(&graph).unwrap();
        assert_eq!(optimizer.cache_size(), 1);

        optimizer.clear_cache();
        assert_eq!(optimizer.cache_size(), 0);
    }

    #[test]
    fn test_checksum_deterministic() {
        let graph = create_linear_graph();

        // Compute checksum twice - should be identical
        let checksum1 = checksum::compute_checksum(&graph);
        let checksum2 = checksum::compute_checksum(&graph);

        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_graph() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);

        let checksum1 = checksum::compute_checksum(&graph);

        // Add another processor
        graph.add_processor("proc_1".into(), "TestProcessor".into(), 0);

        let checksum2 = checksum::compute_checksum(&graph);

        assert_ne!(checksum1, checksum2);
    }

    #[test]
    fn test_execution_plan_to_json() {
        let graph = create_linear_graph();
        let mut optimizer = GraphOptimizer::new();

        let plan = optimizer.optimize(&graph).unwrap();
        let json = plan.to_json();

        assert_eq!(json["variant"], "Legacy");
        assert!(json["processors"].is_array());
        assert!(json["connections"].is_array());
        assert_eq!(json["processors"].as_array().unwrap().len(), 3);
        assert_eq!(json["connections"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_optimizer_stats() {
        let graph = create_linear_graph();
        let optimizer = GraphOptimizer::new();

        let stats = optimizer.stats(&graph);

        assert_eq!(stats.processor_count, 3);
        assert_eq!(stats.connection_count, 2);
        assert_eq!(stats.source_count, 1);
        assert_eq!(stats.sink_count, 1);
        assert!(!stats.cache_hit);
    }

    #[test]
    fn test_config_checksum() {
        #[derive(Debug)]
        struct Config {
            value: i32,
            name: String,
        }

        let config1 = Config {
            value: 42,
            name: "test".into(),
        };
        let config2 = Config {
            value: 42,
            name: "test".into(),
        };
        let config3 = Config {
            value: 99,
            name: "test".into(),
        };

        let checksum1 = compute_config_checksum(&config1);
        let checksum2 = compute_config_checksum(&config2);
        let checksum3 = compute_config_checksum(&config3);

        // Same config should produce same checksum
        assert_eq!(checksum1, checksum2);

        // Different config should produce different checksum
        assert_ne!(checksum1, checksum3);
    }
}
