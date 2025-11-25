//! Graph representation using petgraph
//!
//! The Graph is the "DOM" - a pure data representation of the desired processor topology.
//! It can be serialized, compared, cloned, and analyzed without any execution state.
//!
//! # Design
//!
//! The Graph follows a DOM/VDOM pattern:
//! - **Graph (DOM)**: Pure data structure describing topology
//! - **Executor**: Reads the graph and creates execution state
//!
//! The runtime modifies the Graph, and the executor reads it via shared `Arc<RwLock<Graph>>`.

mod edge;
mod node;
mod validation;

pub use edge::ConnectionEdge;
pub use node::{ProcessorId, ProcessorNode};

use crate::core::bus::connection_id::__private::new_unchecked as new_connection_id;
use crate::core::bus::{ConnectionId, PortType};
use crate::core::error::{Result, StreamError};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for generating unique processor IDs
static PROCESSOR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Global counter for generating unique connection IDs
static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique processor ID
fn generate_processor_id(processor_type: &str) -> ProcessorId {
    let id = PROCESSOR_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}_{}", processor_type.to_lowercase(), id)
}

/// Generate a unique connection ID
fn generate_connection_id() -> ConnectionId {
    let id = CONNECTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    new_connection_id(format!("conn_{}", id))
}

/// Graph represents the desired processor topology (DAG)
///
/// This is a pure data structure that can be:
/// - Serialized to JSON for persistence/debugging
/// - Compared for equality (for delta computation)
/// - Cloned for snapshot/rollback
/// - Analyzed with petgraph algorithms
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    /// petgraph directed graph
    #[serde(skip)]
    graph: DiGraph<ProcessorNode, ConnectionEdge>,

    /// Map processor ID to graph node index
    #[serde(skip)]
    processor_to_node: HashMap<ProcessorId, NodeIndex>,

    /// Serializable representation of nodes (for serde)
    nodes: Vec<ProcessorNode>,

    /// Serializable representation of edges (for serde)
    edges: Vec<ConnectionEdge>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            processor_to_node: HashMap::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Add processor to graph (internal API with explicit ID)
    pub fn add_processor(&mut self, id: ProcessorId, processor_type: String, config_checksum: u64) {
        let node = ProcessorNode {
            id: id.clone(),
            processor_type,
            config_checksum,
        };

        let node_idx = self.graph.add_node(node.clone());
        self.processor_to_node.insert(id, node_idx);
        self.nodes.push(node);
    }

    // =========================================================================
    // Runtime API - These methods are called by StreamRuntime
    // =========================================================================

    /// Add a processor node to the graph
    ///
    /// This is the primary API for adding processors from the runtime.
    /// Generates a unique ID and returns the ProcessorNode (pure data).
    pub fn add_processor_node(&mut self, processor_type: &str) -> ProcessorNode {
        let id = generate_processor_id(processor_type);
        let node = ProcessorNode::new(id.clone(), processor_type.to_string());

        let node_idx = self.graph.add_node(node.clone());
        self.processor_to_node.insert(id, node_idx);
        self.nodes.push(node.clone());

        node
    }

    /// Add a connection edge to the graph
    ///
    /// This is the primary API for adding connections from the runtime.
    /// Port addresses should be in format "processor_id.port_name".
    /// Returns the ConnectionEdge (pure data).
    pub fn add_connection_edge(&mut self, from_port: &str, to_port: &str) -> ConnectionEdge {
        let id = generate_connection_id();
        let edge = ConnectionEdge {
            id: id.clone(),
            from_port: from_port.to_string(),
            to_port: to_port.to_string(),
            port_type: PortType::Data, // Default, can be refined later
        };

        // Parse processor IDs and add edge to petgraph
        if let (Some(source_proc_id), Some(dest_proc_id)) = (
            from_port.split_once('.').map(|(p, _)| p),
            to_port.split_once('.').map(|(p, _)| p),
        ) {
            if let (Some(&from_idx), Some(&to_idx)) = (
                self.processor_to_node.get(source_proc_id),
                self.processor_to_node.get(dest_proc_id),
            ) {
                self.graph.add_edge(from_idx, to_idx, edge.clone());
            }
        }

        self.edges.push(edge.clone());
        edge
    }

    /// Remove a processor node from the graph
    ///
    /// Also removes any connections involving this processor.
    pub fn remove_processor_node(&mut self, id: &ProcessorId) {
        self.remove_processor(id);
    }

    /// Remove a connection edge from the graph
    pub fn remove_connection_edge(&mut self, id: &ConnectionId) {
        self.remove_connection(id);
    }

    // =========================================================================
    // Internal API
    // =========================================================================

    /// Check if a processor exists in the graph
    pub fn has_processor(&self, id: &ProcessorId) -> bool {
        self.processor_to_node.contains_key(id)
    }

    /// Get a processor node by ID
    pub fn get_processor(&self, id: &ProcessorId) -> Option<&ProcessorNode> {
        self.processor_to_node
            .get(id)
            .map(|&idx| &self.graph[idx])
    }

    /// Remove processor from graph (also removes connected edges)
    pub fn remove_processor(&mut self, id: &ProcessorId) {
        if let Some(node_idx) = self.processor_to_node.remove(id) {
            self.graph.remove_node(node_idx);
            self.nodes.retain(|n| &n.id != id);
            // Also remove edges that reference this processor
            self.edges
                .retain(|e| !e.from_port.starts_with(id) && !e.to_port.starts_with(id));
        }
    }

    /// Add connection to graph (simple API - generates connection ID)
    ///
    /// This is the primary API for adding connections from the runtime.
    /// Port addresses should be in format "processor_id.port_name".
    pub fn add_connection(&mut self, from_port: String, to_port: String) -> ConnectionId {
        let id = generate_connection_id();
        // Default to Data port type - can be refined later if needed
        let _ = self.add_connection_with_id(id.clone(), from_port, to_port, PortType::Data);
        id
    }

    /// Add connection to graph with explicit ID and port type
    ///
    /// This is the detailed API for when you need full control over the connection.
    pub fn add_connection_with_id(
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
                id: id.clone(),
                from_port,
                to_port,
                port_type,
            };

            self.graph.add_edge(from_idx, to_idx, edge.clone());
            self.edges.push(edge);
            Ok(())
        } else {
            Err(StreamError::ProcessorNotFound(format!(
                "{} or {}",
                source_proc_id, dest_proc_id
            )))
        }
    }

    /// Find a connection by source and destination ports
    ///
    /// Returns the connection ID if found.
    pub fn find_connection(&self, from_port: &str, to_port: &str) -> Option<ConnectionId> {
        self.find_connection_by_ports(from_port, to_port)
    }

    /// Remove connection from graph
    pub fn remove_connection(&mut self, connection_id: &ConnectionId) {
        if let Some(edge_idx) = self
            .graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *connection_id)
        {
            self.graph.remove_edge(edge_idx);
            self.edges.retain(|e| &e.id != connection_id);
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
    /// ```rust,ignore
    /// let json = runtime.graph().read().to_json();
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

    /// Get the number of processors in the graph
    pub fn processor_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get the number of connections in the graph
    pub fn connection_count(&self) -> usize {
        self.graph.edge_count()
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

    /// Find a connection by its source and destination port addresses
    ///
    /// Port addresses are in format "processor_id.port_name".
    /// Returns the ConnectionId if found, None otherwise.
    pub fn find_connection_by_ports(&self, from_port: &str, to_port: &str) -> Option<ConnectionId> {
        self.graph
            .edge_indices()
            .find(|&e| {
                let edge = &self.graph[e];
                edge.from_port == from_port && edge.to_port == to_port
            })
            .map(|e| self.graph[e].id.clone())
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for Graph {
    fn eq(&self, other: &Self) -> bool {
        // Compare using serializable nodes/edges for equality
        // This is used for delta computation
        self.nodes == other.nodes && self.edges == other.edges
    }
}

impl Eq for Graph {}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(graph.has_processor(&"proc_0".into()));
        assert!(!graph.has_processor(&"unknown".into()));
    }

    #[test]
    fn test_linear_pipeline() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("transform".into(), "TransformProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        // Connect: source -> transform -> sink (simple API)
        graph.add_connection("source.output".into(), "transform.input".into());
        graph.add_connection("transform.output".into(), "sink.input".into());

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
        graph.add_connection("source.output".into(), "sink_a.input".into());
        graph.add_connection("source.output".into(), "sink_b.input".into());

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
        graph.add_connection("a.output".into(), "b.input".into());
        graph.add_connection("b.output".into(), "c.input".into());
        graph.add_connection("c.output".into(), "a.input".into());

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

        let conn_id = graph.add_connection("source.output".into(), "sink.input".into());

        assert_eq!(graph.petgraph().edge_count(), 1);

        graph.remove_connection(&conn_id);
        assert_eq!(graph.petgraph().edge_count(), 0);
    }

    #[test]
    fn test_to_json() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 42);
        graph.add_processor("proc_1".into(), "OtherProcessor".into(), 123);

        graph.add_connection("proc_0.output".into(), "proc_1.input".into());

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

        graph.add_connection("camera.video_out".into(), "display.video_in".into());

        let dot = graph.to_dot();

        // DOT format should contain digraph keyword
        assert!(dot.contains("digraph"));
    }

    #[test]
    fn test_invalid_port_address() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);
        graph.add_processor("proc_1".into(), "TestProcessor".into(), 0);

        // Missing dot separator - should fail (use detailed API)
        let result = graph.add_connection_with_id(
            new_connection_id("conn_1"),
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

        // Connect to non-existent processor (use detailed API)
        let result = graph.add_connection_with_id(
            new_connection_id("conn_1"),
            "proc_0.output".into(),
            "unknown.input".into(), // Processor doesn't exist
            PortType::Video,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_find_connection() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        let conn_id = graph.add_connection("source.output".into(), "sink.input".into());

        // Find by ports
        let found = graph.find_connection("source.output", "sink.input");
        assert_eq!(found, Some(conn_id.clone()));

        // Find by ID
        let edge = graph.find_connection_by_id(&conn_id);
        assert!(edge.is_some());
        assert_eq!(edge.unwrap().from_port, "source.output");
    }

    #[test]
    fn test_graph_equality() {
        let mut graph1 = Graph::new();
        graph1.add_processor("a".into(), "TestProcessor".into(), 0);

        let mut graph2 = Graph::new();
        graph2.add_processor("a".into(), "TestProcessor".into(), 0);

        assert_eq!(graph1, graph2);

        // Different processor makes them unequal
        graph2.add_processor("b".into(), "TestProcessor".into(), 0);
        assert_ne!(graph1, graph2);
    }
}
