use crate::core::error::{Result, StreamError};
use crate::core::link_channel::link_id::__private::new_unchecked as new_link_id;
use crate::core::link_channel::{LinkId, LinkPortType};
use crate::core::processors::Processor;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use super::link::{Link, LinkDirection};
use super::link_port_ref::IntoLinkPortRef;
use super::node::{PortInfo, ProcessorId, ProcessorNode};
use super::validation;

static PROCESSOR_COUNTER: AtomicU64 = AtomicU64::new(0);
static LINK_COUNTER: AtomicU64 = AtomicU64::new(0);

fn generate_processor_id(processor_type: &str) -> ProcessorId {
    let id = PROCESSOR_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}_{}", processor_type.to_lowercase(), id)
}

fn generate_link_id() -> LinkId {
    let id = LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
    new_link_id(format!("link_{}", id))
}

/// Processor topology graph (DAG).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    #[serde(skip)]
    graph: DiGraph<ProcessorNode, Link>,
    #[serde(skip)]
    processor_to_node: HashMap<ProcessorId, NodeIndex>,
    nodes: Vec<ProcessorNode>,
    links: Vec<Link>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            processor_to_node: HashMap::new(),
            nodes: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn add_processor(
        &mut self,
        id: ProcessorId,
        processor_type: String,
        _config_checksum: u64,
    ) {
        let node = ProcessorNode::new(id.clone(), processor_type, None, vec![], vec![]);

        let node_idx = self.graph.add_node(node.clone());
        self.processor_to_node.insert(id, node_idx);
        self.nodes.push(node);
    }

    /// Add a processor node to the graph.
    pub fn add_processor_node<P>(&mut self, config: P::Config) -> Result<ProcessorNode>
    where
        P: Processor + 'static,
        P::Config: serde::Serialize,
    {
        // Get processor descriptor for type name and port metadata
        let descriptor = <P as Processor>::descriptor().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!(
                "Processor {} has no descriptor",
                std::any::type_name::<P>()
            ))
        })?;

        // Extract port info from descriptor
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        // Serialize config to JSON
        let config_json = serde_json::to_value(&config).ok();

        // Create the node
        let id = generate_processor_id(&descriptor.name);
        let node = ProcessorNode::new(
            id.clone(),
            descriptor.name.clone(),
            config_json,
            inputs,
            outputs,
        );

        let node_idx = self.graph.add_node(node.clone());
        self.processor_to_node.insert(id, node_idx);
        self.nodes.push(node.clone());

        Ok(node)
    }

    /// Add a link between two ports.
    pub fn add_link(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<Link> {
        // Convert to LinkPortRef (strings get direction from context)
        let from = from.into_link_port_ref(LinkDirection::Output)?;
        let to = to.into_link_port_ref(LinkDirection::Input)?;

        // Validate directions
        if from.direction != LinkDirection::Output {
            return Err(StreamError::InvalidLink(format!(
                "Source port '{}' must be an output, not an input",
                from.to_address()
            )));
        }
        if to.direction != LinkDirection::Input {
            return Err(StreamError::InvalidLink(format!(
                "Destination port '{}' must be an input, not an output",
                to.to_address()
            )));
        }

        let from_addr = from.to_address();
        let to_addr = to.to_address();

        let id = generate_link_id();
        let link = Link::new(id, &from_addr, &to_addr);

        // Parse processor IDs and add edge to petgraph
        if let (Some(&from_idx), Some(&to_idx)) = (
            self.processor_to_node.get(&link.source.node),
            self.processor_to_node.get(&link.target.node),
        ) {
            self.graph.add_edge(from_idx, to_idx, link.clone());
        }

        self.links.push(link.clone());
        Ok(link)
    }

    /// Remove a processor node and its links.
    pub fn remove_processor_node(&mut self, id: &ProcessorId) {
        self.remove_processor(id);
    }

    pub fn remove_link(&mut self, id: &LinkId) {
        if let Some(edge_idx) = self.graph.edge_indices().find(|&e| self.graph[e].id == *id) {
            self.graph.remove_edge(edge_idx);
            self.links.retain(|l| &l.id != id);
        }
    }

    pub fn has_processor(&self, id: &ProcessorId) -> bool {
        self.processor_to_node.contains_key(id)
    }

    pub fn get_processor(&self, id: &ProcessorId) -> Option<&ProcessorNode> {
        self.processor_to_node.get(id).map(|&idx| &self.graph[idx])
    }

    pub fn remove_processor(&mut self, id: &ProcessorId) {
        if let Some(node_idx) = self.processor_to_node.remove(id) {
            self.graph.remove_node(node_idx);
            self.nodes.retain(|n| &n.id != id);
            // Also remove links that reference this processor
            self.links
                .retain(|l| l.source.node != *id && l.target.node != *id);
        }
    }

    /// Add link by port address strings ("processor_id.port_name").
    pub fn add_link_by_address(&mut self, from_port: String, to_port: String) -> LinkId {
        let id = generate_link_id();
        let _ = self.add_link_with_id(id.clone(), from_port, to_port, LinkPortType::Data);
        id
    }

    pub fn add_link_with_id(
        &mut self,
        id: LinkId,
        from_port: String,
        to_port: String,
        _port_type: LinkPortType,
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
            let link = Link::new(id, &from_port, &to_port);

            self.graph.add_edge(from_idx, to_idx, link.clone());
            self.links.push(link);
            Ok(())
        } else {
            Err(StreamError::ProcessorNotFound(format!(
                "{} or {}",
                source_proc_id, dest_proc_id
            )))
        }
    }

    pub fn find_link(&self, from_port: &str, to_port: &str) -> Option<LinkId> {
        self.find_link_by_ports(from_port, to_port)
    }

    pub fn validate(&self) -> Result<()> {
        validation::validate_graph(&self.graph)
    }

    pub fn to_dot(&self) -> String {
        use petgraph::dot::{Config, Dot};
        format!(
            "{:?}",
            Dot::with_config(&self.graph, &[Config::EdgeNoLabel])
        )
    }

    pub fn to_json(&self) -> serde_json::Value {
        // Note: to_json() is now redundant since Graph implements Serialize
        // This method is kept for backwards compatibility but just delegates to serde
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Get all processors in topological order.
    pub fn topological_order(&self) -> Result<Vec<ProcessorId>> {
        use petgraph::algo::toposort;

        let sorted = toposort(&self.graph, None)
            .map_err(|_| StreamError::InvalidGraph("Graph contains cycles".into()))?;

        Ok(sorted
            .into_iter()
            .map(|idx| self.graph[idx].id.clone())
            .collect())
    }

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

    pub(crate) fn petgraph(&self) -> &DiGraph<ProcessorNode, Link> {
        &self.graph
    }

    pub fn processor_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn link_count(&self) -> usize {
        self.graph.edge_count()
    }

    pub fn find_link_by_id(&self, link_id: &LinkId) -> Option<&Link> {
        self.graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *link_id)
            .map(|e| &self.graph[e])
    }

    pub fn find_link_by_ports(&self, from_port: &str, to_port: &str) -> Option<LinkId> {
        self.graph
            .edge_indices()
            .find(|&e| {
                let link = &self.graph[e];
                link.from_port() == from_port && link.to_port() == to_port
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
        // Compare using serializable nodes/links for equality
        // This is used for delta computation
        self.nodes == other.nodes && self.links == other.links
    }
}

impl Eq for Graph {}

/// Checksum of a graph's structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphChecksum(pub u64);

impl Graph {
    /// Compute deterministic checksum of graph structure.
    pub fn checksum(&self) -> GraphChecksum {
        let mut hasher = DefaultHasher::new();

        // Hash all nodes (sorted by ID for determinism)
        let mut nodes: Vec<_> = self.graph.node_indices().collect();
        nodes.sort_by_key(|&idx| &self.graph[idx].id);

        for node_idx in nodes {
            let node = &self.graph[node_idx];
            node.id.hash(&mut hasher);
            node.processor_type.hash(&mut hasher);
            // Hash config JSON if present
            if let Some(config) = &node.config {
                config.to_string().hash(&mut hasher);
            }
        }

        // Hash all links (sorted by ID for determinism)
        let mut edges: Vec<_> = self.graph.edge_indices().collect();
        edges.sort_by_key(|&idx| &self.graph[idx].id);

        for edge_idx in edges {
            let link = &self.graph[edge_idx];
            link.id.hash(&mut hasher);
            link.from_port().hash(&mut hasher);
            link.to_port().hash(&mut hasher);
        }

        GraphChecksum(hasher.finish())
    }
}

/// Compute a checksum from any Debug-able config.
pub fn compute_config_checksum<T: std::fmt::Debug>(config: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{:?}", config).hash(&mut hasher);
    hasher.finish()
}

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

        // Connect: source -> transform -> sink
        graph.add_link_by_address("source.output".into(), "transform.input".into());
        graph.add_link_by_address("transform.output".into(), "sink.input".into());

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
        graph.add_link_by_address("source.output".into(), "sink_a.input".into());
        graph.add_link_by_address("source.output".into(), "sink_b.input".into());

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
        graph.add_link_by_address("a.output".into(), "b.input".into());
        graph.add_link_by_address("b.output".into(), "c.input".into());
        graph.add_link_by_address("c.output".into(), "a.input".into());

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
    fn test_remove_link() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        let link_id = graph.add_link_by_address("source.output".into(), "sink.input".into());

        assert_eq!(graph.petgraph().edge_count(), 1);

        graph.remove_link(&link_id);
        assert_eq!(graph.petgraph().edge_count(), 0);
    }

    #[test]
    fn test_to_json() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 42);
        graph.add_processor("proc_1".into(), "OtherProcessor".into(), 123);

        graph.add_link_by_address("proc_0.output".into(), "proc_1.input".into());

        let json = graph.to_json();

        // Verify nodes
        let nodes = json["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);

        // Verify links
        let links = json["links"].as_array().unwrap();
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_to_dot() {
        let mut graph = Graph::new();
        graph.add_processor("camera".into(), "CameraProcessor".into(), 0);
        graph.add_processor("display".into(), "DisplayProcessor".into(), 0);

        graph.add_link_by_address("camera.video_out".into(), "display.video_in".into());

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
        let result = graph.add_link_with_id(
            new_link_id("link_1"),
            "proc_0output".into(), // Missing dot
            "proc_1.input".into(),
            LinkPortType::Video,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_link_to_unknown_processor() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);

        // Link to non-existent processor
        let result = graph.add_link_with_id(
            new_link_id("link_1"),
            "proc_0.output".into(),
            "unknown.input".into(), // Processor doesn't exist
            LinkPortType::Video,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_find_link() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        let link_id = graph.add_link_by_address("source.output".into(), "sink.input".into());

        // Find by ports
        let found = graph.find_link("source.output", "sink.input");
        assert_eq!(found, Some(link_id.clone()));

        // Find by ID
        let link = graph.find_link_by_id(&link_id);
        assert!(link.is_some());
        assert_eq!(link.unwrap().from_port(), "source.output");
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

    #[test]
    fn test_checksum_deterministic() {
        let mut graph = Graph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);
        graph.add_link_by_address("source.output".into(), "sink.input".into());

        // Compute checksum twice - should be identical
        let checksum1 = graph.checksum();
        let checksum2 = graph.checksum();

        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_graph() {
        let mut graph = Graph::new();
        graph.add_processor("proc_0".into(), "TestProcessor".into(), 0);

        let checksum1 = graph.checksum();

        // Add another processor
        graph.add_processor("proc_1".into(), "TestProcessor".into(), 0);

        let checksum2 = graph.checksum();

        assert_ne!(checksum1, checksum2);
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
