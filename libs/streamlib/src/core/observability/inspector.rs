// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph inspection for runtime observation.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::graph::{
    Graph, GraphNode, GraphState, LinkUniqueId, ProcessorMetrics, ProcessorUniqueId, StateComponent,
};

use super::snapshots::{
    GraphHealth, GraphStateSnapshot, LatencyStats, LinkSnapshot, ProcessorSnapshot,
};

/// Inspector for observing graph state without mutation.
///
/// Provides read-only access to graph topology and runtime metrics.
/// All methods take shared references and are safe to call from any thread.
pub struct GraphInspector {
    graph: Arc<RwLock<Graph>>,
}

impl GraphInspector {
    /// Create a new inspector for a Graph.
    pub fn new(graph: Arc<RwLock<Graph>>) -> Self {
        Self { graph }
    }

    /// Get a snapshot of a specific processor.
    pub fn processor(&self, id: &NodeIndex) -> Option<ProcessorSnapshot> {
        let graph = self.graph.read();

        let node = graph.query().V_id(id).first()?;

        // Get state from node's component storage if available
        let state = node
            .get::<StateComponent>()
            .map(|s| *s.0.lock())
            .unwrap_or_default();

        // Get metrics from node's component storage if available
        let metrics = node.get::<ProcessorMetrics>().cloned().unwrap_or_default();

        Some(ProcessorSnapshot {
            id: id.clone(),
            processor_type: node.processor_type.clone(),
            state,
            throughput_fps: metrics.throughput_fps,
            latency: LatencyStats {
                p50: std::time::Duration::from_secs_f64(metrics.latency_p50_ms / 1000.0),
                p90: std::time::Duration::ZERO, // Not tracked yet
                p99: std::time::Duration::from_secs_f64(metrics.latency_p99_ms / 1000.0),
                max: std::time::Duration::ZERO, // Not tracked yet
            },
            config: node.config.clone().unwrap_or(serde_json::Value::Null),
        })
    }

    /// Get a snapshot of a specific link.
    pub fn link(&self, id: &LinkUniqueId) -> Option<LinkSnapshot> {
        let graph = self.graph.read();

        let link = graph.query().E_id(id).first()?;

        Some(LinkSnapshot {
            id: id.clone(),
            source_processor: link.source.node.clone(),
            source_port: link.source.port.clone(),
            target_processor: link.target.node.clone(),
            target_port: link.target.port.clone(),
            queue_depth: 0,      // TODO: Get from link metrics component
            capacity: 16,        // Default capacity
            throughput_fps: 0.0, // TODO: Get from link metrics component
        })
    }

    /// Get overall graph health summary.
    pub fn health(&self) -> GraphHealth {
        let graph = self.graph.read();

        let processor_count = graph.query().v().count();
        let link_count = graph.query().e().count();

        // Aggregate metrics from all processors
        let mut total_dropped = 0u64;
        let mut bottlenecks = Vec::new();

        for node in graph.query().v().iter() {
            if let Some(metrics) = node.get::<ProcessorMetrics>() {
                total_dropped += metrics.frames_dropped;

                // Simple bottleneck detection: high drop rate
                if metrics.frames_processed > 0 {
                    let drop_rate = metrics.frames_dropped as f64 / metrics.frames_processed as f64;
                    if drop_rate > 0.01 {
                        // More than 1% drops
                        bottlenecks.push(node.id.clone());
                    }
                }
            }
        }

        GraphHealth {
            state: convert_graph_state(graph.state()),
            processor_count,
            link_count,
            dropped_frames: total_dropped,
            error_count: 0, // TODO: Track errors in components
            bottlenecks,
        }
    }

    /// List all processor IDs.
    pub fn processor_ids(&self) -> Vec<NodeIndex> {
        let graph = self.graph.read();
        graph.query().v().ids()
    }

    /// List all link IDs.
    pub fn link_ids(&self) -> Vec<LinkUniqueId> {
        let graph = self.graph.read();
        graph.query().e().ids()
    }

    /// Get the current graph state.
    pub fn state(&self) -> GraphStateSnapshot {
        let graph = self.graph.read();
        convert_graph_state(graph.state())
    }

    /// Check if the graph is running.
    pub fn is_running(&self) -> bool {
        self.state() == GraphStateSnapshot::Running
    }

    /// Get processor count.
    pub fn processor_count(&self) -> usize {
        let graph = self.graph.read();
        graph.query().v().count()
    }

    /// Get link count.
    pub fn link_count(&self) -> usize {
        let graph = self.graph.read();
        graph.query().e().count()
    }
}

/// Convert internal GraphState to snapshot-friendly enum.
fn convert_graph_state(state: GraphState) -> GraphStateSnapshot {
    match state {
        GraphState::Idle => GraphStateSnapshot::Idle,
        GraphState::Running => GraphStateSnapshot::Running,
        GraphState::Paused => GraphStateSnapshot::Paused,
        GraphState::Stopping => GraphStateSnapshot::Idle, // Map stopping to idle for observers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inspector_creation() {
        let graph = Arc::new(RwLock::new(Graph::new()));

        let inspector = GraphInspector::new(graph);
        assert_eq!(inspector.processor_count(), 0);
        assert_eq!(inspector.link_count(), 0);
        assert_eq!(inspector.state(), GraphStateSnapshot::Idle);
    }

    #[test]
    fn test_inspector_health() {
        let graph = Arc::new(RwLock::new(Graph::new()));

        let inspector = GraphInspector::new(graph);
        let health = inspector.health();

        assert!(health.is_healthy());
        assert_eq!(health.processor_count, 0);
        assert_eq!(health.link_count, 0);
        assert_eq!(health.dropped_frames, 0);
    }
}
