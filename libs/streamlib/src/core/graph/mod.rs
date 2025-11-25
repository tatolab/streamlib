//! Graph representation using petgraph
//!
//! The graph is the source of truth for desired processor topology.

mod edge;
mod node;
mod validation;

pub use edge::ConnectionEdge;
pub use node::ProcessorNode;

use crate::core::bus::{ConnectionId, PortType};
use crate::core::error::{Result, StreamError};
use crate::core::handles::ProcessorId;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use std::collections::HashMap;

/// Graph represents the desired processor topology (DAG)
pub struct Graph {
    /// petgraph directed graph
    graph: DiGraph<ProcessorNode, ConnectionEdge>,

    /// Map processor ID to graph node index
    processor_to_node: HashMap<ProcessorId, NodeIndex>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            processor_to_node: HashMap::new(),
        }
    }

    /// Add processor to graph
    pub fn add_processor(&mut self, id: ProcessorId, processor_type: String, config_checksum: u64) {
        let node = ProcessorNode {
            id: id.clone(),
            processor_type,
            config_checksum,
        };

        let node_idx = self.graph.add_node(node);
        self.processor_to_node.insert(id, node_idx);
    }

    /// Remove processor from graph
    pub fn remove_processor(&mut self, id: &ProcessorId) {
        if let Some(node_idx) = self.processor_to_node.remove(id) {
            self.graph.remove_node(node_idx);
        }
    }

    /// Add connection to graph
    pub fn add_connection(
        &mut self,
        id: ConnectionId,
        from_port: String,
        to_port: String,
        port_type: PortType,
    ) -> Result<()> {
        // Parse processor IDs from port addresses
        let (source_proc_id, _source_port_name) = from_port
            .split_once('.')
            .ok_or_else(|| StreamError::InvalidPortAddress(from_port.clone()))?;
        let (dest_proc_id, _dest_port_name) = to_port
            .split_once('.')
            .ok_or_else(|| StreamError::InvalidPortAddress(to_port.clone()))?;

        let from_node = self.processor_to_node.get(source_proc_id);
        let to_node = self.processor_to_node.get(dest_proc_id);

        if let (Some(&from_idx), Some(&to_idx)) = (from_node, to_node) {
            let edge = ConnectionEdge {
                id,
                from_port,
                to_port,
                port_type,
            };

            self.graph.add_edge(from_idx, to_idx, edge);
            Ok(())
        } else {
            Err(StreamError::ProcessorNotFound(format!(
                "{} or {}",
                source_proc_id, dest_proc_id
            )))
        }
    }

    /// Remove connection from graph
    pub fn remove_connection(&mut self, connection_id: &ConnectionId) {
        if let Some(edge_idx) = self
            .graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *connection_id)
        {
            self.graph.remove_edge(edge_idx);
        }
    }

    /// Validate graph (check for cycles, type mismatches, etc.)
    pub fn validate(&self) -> Result<()> {
        validation::validate_graph(&self.graph)
    }

    /// Export graph as DOT format for Graphviz visualization
    pub fn to_dot(&self) -> String {
        use petgraph::dot::{Config, Dot};
        format!(
            "{:?}",
            Dot::with_config(&self.graph, &[Config::EdgeNoLabel])
        )
    }

    /// Export graph as JSON for web visualization and testing
    ///
    /// This method is critical for:
    /// 1. **Testing**: Assert exact graph structure in tests
    /// 2. **Debugging**: Inspect graph state during development
    /// 3. **Visualization**: Render graph in web UI or tools
    /// 4. **Snapshot testing**: Compare graph before/after mutations
    ///
    /// # Example Test Usage
    /// ```rust
    /// let json = runtime.graph().to_json();
    /// assert_eq!(json["nodes"].as_array().unwrap().len(), 3);
    /// assert_eq!(json["edges"].as_array().unwrap().len(), 2);
    ///
    /// // Verify specific node exists
    /// let camera_node = json["nodes"].as_array().unwrap()
    ///     .iter()
    ///     .find(|n| n["id"] == "camera_1")
    ///     .expect("camera node not found");
    /// assert_eq!(camera_node["type"], "CameraProcessor");
    ///
    /// // Verify specific connection exists
    /// let connection = json["edges"].as_array().unwrap()
    ///     .iter()
    ///     .find(|e| e["from"] == "camera_1" && e["to"] == "display_1")
    ///     .expect("connection not found");
    /// assert_eq!(connection["from_port"], "camera_1.video");
    /// assert_eq!(connection["to_port"], "display_1.input");
    /// ```
    pub fn to_json(&self) -> serde_json::Value {
        let nodes: Vec<_> = self
            .graph
            .node_indices()
            .map(|idx| {
                let node = &self.graph[idx];
                serde_json::json!({
                    "id": node.id,
                    "type": node.processor_type,
                    "checksum": node.config_checksum,
                })
            })
            .collect();

        let edges: Vec<_> = self
            .graph
            .edge_indices()
            .map(|idx| {
                let edge = &self.graph[idx];
                let (from, to) = self.graph.edge_endpoints(idx).unwrap();
                serde_json::json!({
                    "id": edge.id,
                    "from": self.graph[from].id,
                    "to": self.graph[to].id,
                    "from_port": edge.from_port,
                    "to_port": edge.to_port,
                    "port_type": format!("{:?}", edge.port_type),
                })
            })
            .collect();

        serde_json::json!({
            "nodes": nodes,
            "edges": edges,
        })
    }

    /// Get all processors in topological order (sources first)
    pub fn topological_order(&self) -> Result<Vec<ProcessorId>> {
        use petgraph::algo::toposort;

        let sorted = toposort(&self.graph, None)
            .map_err(|_| StreamError::InvalidGraph("Graph contains cycles".into()))?;

        Ok(sorted
            .into_iter()
            .map(|idx| self.graph[idx].id.clone())
            .collect())
    }

    /// Find all source processors (no incoming connections)
    pub fn find_sources(&self) -> Vec<ProcessorId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .count()
                    == 0
            })
            .map(|idx| self.graph[idx].id.clone())
            .collect()
    }

    /// Find all sink processors (no outgoing connections)
    pub fn find_sinks(&self) -> Vec<ProcessorId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, Direction::Outgoing)
                    .count()
                    == 0
            })
            .map(|idx| self.graph[idx].id.clone())
            .collect()
    }

    /// Get petgraph reference (for optimizer)
    pub(crate) fn petgraph(&self) -> &DiGraph<ProcessorNode, ConnectionEdge> {
        &self.graph
    }

    /// Find a connection by its ID
    ///
    /// Returns the edge data if found, None otherwise.
    pub fn find_connection_by_id(&self, connection_id: &ConnectionId) -> Option<&ConnectionEdge> {
        self.graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *connection_id)
            .map(|e| &self.graph[e])
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::connection_id::__private::new_unchecked;

    #[test]
    fn test_empty_graph() {
        let graph = Graph::new();
        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources().len(), 0);
        assert_eq!(graph.find_sinks().len(), 0);
        assert!(graph.topological_order().unwrap().is_empty());
    }

    #[test]
    fn test_single_processor() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 123);

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources(), vec!["proc_0"]);
        assert_eq!(graph.find_sinks(), vec!["proc_0"]);
    }

    #[test]
    fn test_linear_pipeline() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("transform".into(), "TransformProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        // Connect: source -> transform -> sink
        graph
            .add_connection(
                new_unchecked("conn_1"),
                "source.output".into(),
                "transform.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
            .add_connection(
                new_unchecked("conn_2"),
                "transform.output".into(),
                "sink.input".into(),
                PortType::Video,
            )
            .unwrap();

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources(), vec!["source"]);
        assert_eq!(graph.find_sinks(), vec!["sink"]);

        let order = graph.topological_order().unwrap();
        assert_eq!(order.len(), 3);
        // Source must come before transform, transform before sink
        let source_pos = order.iter().position(|x| x == "source").unwrap();
        let transform_pos = order.iter().position(|x| x == "transform").unwrap();
        let sink_pos = order.iter().position(|x| x == "sink").unwrap();
        assert!(source_pos < transform_pos);
        assert!(transform_pos < sink_pos);
    }

    #[test]
    fn test_branching_pipeline() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink_a".into(), "SinkProcessor".into(), 0);
        graph.add_processor("sink_b".into(), "SinkProcessor".into(), 0);

        // source -> sink_a
        // source -> sink_b
        graph
            .add_connection(
                new_unchecked("conn_1"),
                "source.output".into(),
                "sink_a.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
            .add_connection(
                new_unchecked("conn_2"),
                "source.output".into(),
                "sink_b.input".into(),
                PortType::Video,
            )
            .unwrap();

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources(), vec!["source"]);

        let sinks = graph.find_sinks();
        assert_eq!(sinks.len(), 2);
        assert!(sinks.contains(&"sink_a".into()));
        assert!(sinks.contains(&"sink_b".into()));
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = Graph::new();
        graph.add_processor("a".into(), "Processor".into(), 0);
        graph.add_processor("b".into(), "Processor".into(), 0);
        graph.add_processor("c".into(), "Processor".into(), 0);

        // Create cycle: a -> b -> c -> a
        graph
            .add_connection(
                new_unchecked("conn_1"),
                "a.output".into(),
                "b.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
            .add_connection(
                new_unchecked("conn_2"),
                "b.output".into(),
                "c.input".into(),
                PortType::Video,
            )
            .unwrap();

        graph
            .add_connection(
                new_unchecked("conn_3"),
                "c.output".into(),
                "a.input".into(),
                PortType::Video,
            )
            .unwrap();

        // Validation should fail due to cycle
        assert!(graph.validate().is_err());
    }

    #[test]
    fn test_remove_processor() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);
        graph.add_processor("proc_1".into(), "TestProcessor".into(), 0);

        assert_eq!(graph.petgraph().node_count(), 2);

        graph.remove_processor(&"proc_0".into());
        assert_eq!(graph.petgraph().node_count(), 1);
    }

    #[test]
    fn test_remove_connection() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        let conn_id = new_unchecked("conn_1");
        graph
            .add_connection(
                conn_id.clone(),
                "source.output".into(),
                "sink.input".into(),
                PortType::Video,
            )
            .unwrap();

        assert_eq!(graph.petgraph().edge_count(), 1);

        graph.remove_connection(&conn_id);
        assert_eq!(graph.petgraph().edge_count(), 0);
    }

    #[test]
    fn test_to_json() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 42);
        graph.add_processor("proc_1".into(), "OtherProcessor".into(), 123);

        graph
            .add_connection(
                new_unchecked("conn_1"),
                "proc_0.output".into(),
                "proc_1.input".into(),
                PortType::Video,
            )
            .unwrap();

        let json = graph.to_json();

        // Verify nodes
        let nodes = json["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);

        // Verify edges
        let edges = json["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_to_dot() {
        let mut graph = Graph::new();
        graph.add_processor("camera".into(), "CameraProcessor".into(), 0);
        graph.add_processor("display".into(), "DisplayProcessor".into(), 0);

        graph
            .add_connection(
                new_unchecked("conn_1"),
                "camera.video_out".into(),
                "display.video_in".into(),
                PortType::Video,
            )
            .unwrap();

        let dot = graph.to_dot();

        // DOT format should contain digraph keyword
        assert!(dot.contains("digraph"));
    }

    #[test]
    fn test_invalid_port_address() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);
        graph.add_processor("proc_1".into(), "TestProcessor".into(), 0);

        // Missing dot separator - should fail
        let result = graph.add_connection(
            new_unchecked("conn_1"),
            "proc_0output".into(), // Missing dot
            "proc_1.input".into(),
            PortType::Video,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_connection_to_unknown_processor() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);

        // Connect to non-existent processor
        let result = graph.add_connection(
            new_unchecked("conn_1"),
            "proc_0.output".into(),
            "unknown.input".into(), // Processor doesn't exist
            PortType::Video,
        );

        assert!(result.is_err());
    }
}
