// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Result, StreamError};
use crate::core::links::LinkUniqueId;
use crate::core::processors::Processor;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::core::graph::{
    validation, IntoLinkPortRef, Link, LinkDirection, PortInfo, ProcessorId, ProcessorNode,
};

/// Internal processor-link topology graph (DAG).
///
/// This is an internal implementation detail - do not use directly.
/// Use [`Graph`](super::Graph) (the public API) instead.
///
/// The graph is the single source of truth - no secondary indices are maintained.
/// All lookups scan the graph directly to ensure consistency after modifications.
#[derive(Debug)]
pub(crate) struct InternalProcessorLinkGraph {
    graph: DiGraph<ProcessorNode, Link>,
}

/// Serialization helper - stores references to nodes and links.
#[derive(Serialize)]
struct SerializedGraphRef<'a> {
    nodes: Vec<&'a ProcessorNode>,
    links: Vec<&'a Link>,
}

/// Deserialization helper - owns nodes and links.
#[derive(Deserialize)]
struct SerializedGraphOwned {
    nodes: Vec<ProcessorNode>,
    links: Vec<Link>,
}

impl Serialize for InternalProcessorLinkGraph {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let nodes: Vec<_> = self
            .graph
            .node_indices()
            .map(|idx| &self.graph[idx])
            .collect();
        let links: Vec<_> = self
            .graph
            .edge_indices()
            .map(|idx| &self.graph[idx])
            .collect();
        SerializedGraphRef { nodes, links }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InternalProcessorLinkGraph {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let serialized = SerializedGraphOwned::deserialize(deserializer)?;
        let mut graph_impl = InternalProcessorLinkGraph::new();

        // Add all nodes first
        for node in serialized.nodes {
            graph_impl.graph.add_node(node);
        }

        // Then add all links
        for link in serialized.links {
            if let (Some(from_idx), Some(to_idx)) = (
                graph_impl.find_node_index(&link.source.node),
                graph_impl.find_node_index(&link.target.node),
            ) {
                graph_impl.graph.add_edge(from_idx, to_idx, link);
            }
        }

        Ok(graph_impl)
    }
}

/// Checksum of a graph's structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphChecksum(pub u64);

impl InternalProcessorLinkGraph {
    pub(crate) fn new() -> Self {
        Self {
            graph: DiGraph::new(),
        }
    }

    /// Find a node index by processor ID (scans the graph).
    fn find_node_index(&self, id: impl AsRef<str>) -> Option<NodeIndex> {
        let id_str = id.as_ref();
        self.graph
            .node_indices()
            .find(|&idx| self.graph[idx].id.as_str() == id_str)
    }

    /// Add a processor node with the given type. ID is auto-generated.
    pub(crate) fn add_processor(&mut self, processor_type: impl Into<String>) -> &ProcessorNode {
        let node = ProcessorNode::new(processor_type, None, vec![], vec![]);
        let idx = self.graph.add_node(node);
        &self.graph[idx]
    }

    /// Add a processor node to the graph.
    pub(crate) fn add_processor_node<P>(&mut self, config: P::Config) -> Result<&ProcessorNode>
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

        // Create the node (ID is auto-generated)
        let node = ProcessorNode::new(descriptor.name.clone(), config_json, inputs, outputs);

        let node_idx = self.graph.add_node(node);
        Ok(&self.graph[node_idx])
    }

    /// Add a link between two ports. Returns the link ID.
    pub(crate) fn add_link(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<LinkUniqueId> {
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

        let link = Link::new(&from_addr, &to_addr);

        // Find node indices by scanning the graph
        let from_idx = self.find_node_index(&link.source.node);
        let to_idx = self.find_node_index(&link.target.node);

        match (from_idx, to_idx) {
            (Some(from_idx), Some(to_idx)) => {
                let id = link.id.clone();
                self.graph.add_edge(from_idx, to_idx, link);
                Ok(id)
            }
            _ => Err(StreamError::ProcessorNotFound(format!(
                "{} or {}",
                link.source.node, link.target.node
            ))),
        }
    }

    /// Remove a processor node and its links.
    pub(crate) fn remove_processor_node(&mut self, id: impl AsRef<str>) {
        self.remove_processor(id);
    }

    pub(crate) fn remove_link(&mut self, id: &LinkUniqueId) {
        if let Some(edge_idx) = self.graph.edge_indices().find(|&e| self.graph[e].id == *id) {
            self.graph.remove_edge(edge_idx);
        }
    }

    pub(crate) fn has_processor(&self, id: impl AsRef<str>) -> bool {
        self.find_node_index(id).is_some()
    }

    pub(crate) fn get_processor(&self, id: impl AsRef<str>) -> Option<&ProcessorNode> {
        self.find_node_index(id).map(|idx| &self.graph[idx])
    }

    /// Get mutable access to a processor node by ID.
    pub(crate) fn get_processor_mut(&mut self, id: impl AsRef<str>) -> Option<&mut ProcessorNode> {
        self.find_node_index(id.as_ref())
            .map(|idx| &mut self.graph[idx])
    }

    pub(crate) fn remove_processor(&mut self, id: impl AsRef<str>) {
        if let Some(node_idx) = self.find_node_index(id) {
            // Removing a node in DiGraph also removes all edges connected to it
            self.graph.remove_node(node_idx);
        }
    }

    /// Add link by port address strings ("processor_id.port_name").
    pub(crate) fn add_link_by_address(&mut self, from_port: String, to_port: String) -> LinkUniqueId {
        let link = Link::new(&from_port, &to_port);
        let id = link.id.clone();

        // Parse processor IDs from port addresses
        let (source_proc_id, _) = from_port.split_once('.').unwrap_or((&from_port, ""));
        let (dest_proc_id, _) = to_port.split_once('.').unwrap_or((&to_port, ""));

        // Find node indices
        if let (Some(from_idx), Some(to_idx)) = (
            self.find_node_index(source_proc_id),
            self.find_node_index(dest_proc_id),
        ) {
            self.graph.add_edge(from_idx, to_idx, link);
        }

        id
    }

    /// Try to add a link by port addresses, returning an error if invalid.
    pub(crate) fn try_add_link_by_address(
        &mut self,
        from_port: &str,
        to_port: &str,
    ) -> Result<LinkUniqueId> {
        // Parse processor IDs from port addresses
        let (source_proc_id, _source_port_name) = from_port
            .split_once('.')
            .ok_or_else(|| StreamError::InvalidPortAddress(from_port.to_string()))?;
        let (dest_proc_id, _dest_port_name) = to_port
            .split_once('.')
            .ok_or_else(|| StreamError::InvalidPortAddress(to_port.to_string()))?;

        // Find node indices by scanning the graph
        let from_idx = self.find_node_index(source_proc_id);
        let to_idx = self.find_node_index(dest_proc_id);

        match (from_idx, to_idx) {
            (Some(from_idx), Some(to_idx)) => {
                let link = Link::new(from_port, to_port);
                let id = link.id.clone();
                self.graph.add_edge(from_idx, to_idx, link);
                Ok(id)
            }
            _ => Err(StreamError::ProcessorNotFound(format!(
                "{} or {}",
                source_proc_id, dest_proc_id
            ))),
        }
    }

    pub(crate) fn find_link(&self, from_port: &str, to_port: &str) -> Option<LinkUniqueId> {
        self.find_link_by_ports(from_port, to_port)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validation::validate_graph(&self.graph)
    }

    #[allow(dead_code)]
    pub(crate) fn to_dot(&self) -> String {
        use petgraph::dot::{Config, Dot};
        format!(
            "{:?}",
            Dot::with_config(&self.graph, &[Config::EdgeNoLabel])
        )
    }

    #[allow(dead_code)]
    pub(crate) fn to_json(&self) -> serde_json::Value {
        // Note: to_json() is now redundant since Graph implements Serialize
        // This method is kept for backwards compatibility but just delegates to serde
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Get all processors in topological order.
    pub(crate) fn topological_order(&self) -> Result<Vec<ProcessorId>> {
        use petgraph::algo::toposort;

        let sorted = toposort(&self.graph, None)
            .map_err(|_| StreamError::InvalidGraph("Graph contains cycles".into()))?;

        Ok(sorted
            .into_iter()
            .map(|idx| self.graph[idx].id.clone())
            .collect())
    }

    pub(crate) fn find_sources(&self) -> Vec<ProcessorId> {
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

    pub(crate) fn find_sinks(&self) -> Vec<ProcessorId> {
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

    pub(crate) fn processor_count(&self) -> usize {
        self.graph.node_count()
    }

    pub(crate) fn link_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get a link by its ID.
    pub(crate) fn get_link(&self, link_id: &LinkUniqueId) -> Option<&Link> {
        self.graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *link_id)
            .map(|e| &self.graph[e])
    }

    /// Get mutable access to a link by its ID.
    pub(crate) fn get_link_mut(&mut self, link_id: &LinkUniqueId) -> Option<&mut Link> {
        self.graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *link_id)
            .map(|e| &mut self.graph[e])
    }

    /// Get a link by its ID (alias for `get_link` for backwards compatibility).
    #[allow(dead_code)]
    pub(crate) fn find_link_by_id(&self, link_id: &LinkUniqueId) -> Option<&Link> {
        self.get_link(link_id)
    }

    /// Get all processor nodes in the graph.
    pub(crate) fn nodes(&self) -> Vec<&ProcessorNode> {
        self.graph
            .node_indices()
            .map(|idx| &self.graph[idx])
            .collect()
    }

    /// Get all links in the graph.
    pub(crate) fn links(&self) -> Vec<&Link> {
        self.graph
            .edge_indices()
            .map(|idx| &self.graph[idx])
            .collect()
    }

    /// Get the internal petgraph DiGraph.
    pub(crate) fn graph(&self) -> &DiGraph<ProcessorNode, Link> {
        &self.graph
    }

    /// Get the NodeIndex for a processor ID.
    pub(crate) fn processor_to_node_index(&self, id: impl AsRef<str>) -> Option<NodeIndex> {
        self.find_node_index(id)
    }

    pub(crate) fn find_link_by_ports(&self, from_port: &str, to_port: &str) -> Option<LinkUniqueId> {
        self.graph
            .edge_indices()
            .find(|&e| {
                let link = &self.graph[e];
                link.from_port() == from_port && link.to_port() == to_port
            })
            .map(|e| self.graph[e].id.clone())
    }

    /// Update a processor's configuration.
    ///
    /// Returns the old checksum if the processor exists (for delta detection).
    pub(crate) fn update_processor_config(
        &mut self,
        processor_id: impl AsRef<str>,
        config: serde_json::Value,
    ) -> Result<u64> {
        let id_str = processor_id.as_ref();
        let node_idx = self
            .find_node_index(id_str)
            .ok_or_else(|| StreamError::ProcessorNotFound(id_str.to_string().into()))?;

        let node = &mut self.graph[node_idx];
        let old_checksum = node.config_checksum;
        node.set_config(config);

        Ok(old_checksum)
    }

    /// Get a processor's config checksum.
    pub(crate) fn get_processor_config_checksum(
        &self,
        processor_id: impl AsRef<str>,
    ) -> Option<u64> {
        self.find_node_index(processor_id)
            .map(|idx| self.graph[idx].config_checksum)
    }
}

impl Default for InternalProcessorLinkGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for InternalProcessorLinkGraph {
    fn eq(&self, other: &Self) -> bool {
        // Compare node counts first for quick inequality check
        if self.graph.node_count() != other.graph.node_count() {
            return false;
        }
        if self.graph.edge_count() != other.graph.edge_count() {
            return false;
        }

        // Compare all nodes by ID and content
        for idx in self.graph.node_indices() {
            let node = &self.graph[idx];
            match other.find_node_index(&node.id) {
                Some(other_idx) => {
                    if self.graph[idx] != other.graph[other_idx] {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // Compare all edges by ID and content
        for idx in self.graph.edge_indices() {
            let link = &self.graph[idx];
            match other.get_link(&link.id) {
                Some(other_link) => {
                    if link != other_link {
                        return false;
                    }
                }
                None => return false,
            }
        }

        true
    }
}

impl Eq for InternalProcessorLinkGraph {}

impl InternalProcessorLinkGraph {
    /// Compute deterministic checksum of graph structure.
    pub(crate) fn checksum(&self) -> GraphChecksum {
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
        let graph = InternalProcessorLinkGraph::new();
        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources().len(), 0);
        assert_eq!(graph.find_sinks().len(), 0);
        assert!(graph.topological_order().unwrap().is_empty());
    }

    #[test]
    fn test_single_processor() {
        let mut graph = InternalProcessorLinkGraph::new();
        let proc = graph.add_processor("TestProcessor");
        let proc_id = proc.id.clone();

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources().len(), 1);
        assert_eq!(graph.find_sinks().len(), 1);
        assert!(graph.has_processor(&proc_id));
        assert!(!graph.has_processor("unknown"));
    }

    #[test]
    fn test_linear_pipeline() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let transform = graph.add_processor("TransformProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        // Connect: source -> transform -> sink
        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", transform));
        graph.add_link_by_address(format!("{}.output", transform), format!("{}.input", sink));

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources(), vec![source.clone()]);
        assert_eq!(graph.find_sinks(), vec![sink.clone()]);

        let order = graph.topological_order().unwrap();
        assert_eq!(order.len(), 3);
        // Source must come before transform, transform before sink
        let source_pos = order.iter().position(|x| x == &source).unwrap();
        let transform_pos = order.iter().position(|x| x == &transform).unwrap();
        let sink_pos = order.iter().position(|x| x == &sink).unwrap();
        assert!(source_pos < transform_pos);
        assert!(transform_pos < sink_pos);
    }

    #[test]
    fn test_branching_pipeline() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink_a = graph.add_processor("SinkProcessor").id.clone();
        let sink_b = graph.add_processor("SinkProcessor").id.clone();

        // source -> sink_a
        // source -> sink_b
        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink_a));
        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink_b));

        assert!(graph.validate().is_ok());
        assert_eq!(graph.find_sources(), vec![source]);

        let sinks = graph.find_sinks();
        assert_eq!(sinks.len(), 2);
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = InternalProcessorLinkGraph::new();
        let a = graph.add_processor("Processor").id.clone();
        let b = graph.add_processor("Processor").id.clone();
        let c = graph.add_processor("Processor").id.clone();

        // Create cycle: a -> b -> c -> a
        graph.add_link_by_address(format!("{}.output", a), format!("{}.input", b));
        graph.add_link_by_address(format!("{}.output", b), format!("{}.input", c));
        graph.add_link_by_address(format!("{}.output", c), format!("{}.input", a));

        // Validation should fail due to cycle
        assert!(graph.validate().is_err());
    }

    #[test]
    fn test_remove_processor() {
        let mut graph = InternalProcessorLinkGraph::new();
        let proc_0 = graph.add_processor("TestProcessor").id.clone();
        let _proc_1 = graph.add_processor("TestProcessor");

        assert_eq!(graph.processor_count(), 2);

        graph.remove_processor(&proc_0);
        assert_eq!(graph.processor_count(), 1);
    }

    #[test]
    fn test_remove_link() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        let link_id =
            graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink));

        assert_eq!(graph.link_count(), 1);

        graph.remove_link(&link_id);
        assert_eq!(graph.link_count(), 0);
    }

    #[test]
    fn test_to_json() {
        let mut graph = InternalProcessorLinkGraph::new();
        let proc_0 = graph.add_processor("TestProcessor").id.clone();
        let proc_1 = graph.add_processor("OtherProcessor").id.clone();

        graph.add_link_by_address(format!("{}.output", proc_0), format!("{}.input", proc_1));

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
        let mut graph = InternalProcessorLinkGraph::new();
        let camera = graph.add_processor("CameraProcessor").id.clone();
        let display = graph.add_processor("DisplayProcessor").id.clone();

        graph.add_link_by_address(
            format!("{}.video_out", camera),
            format!("{}.video_in", display),
        );

        let dot = graph.to_dot();

        // DOT format should contain digraph keyword
        assert!(dot.contains("digraph"));
    }

    #[test]
    fn test_invalid_port_address() {
        let mut graph = InternalProcessorLinkGraph::new();
        let proc_0 = graph.add_processor("TestProcessor").id.clone();
        let _proc_1 = graph.add_processor("TestProcessor");

        // Missing dot separator - should fail
        let result = graph.try_add_link_by_address(
            &format!("{}output", proc_0), // Missing dot
            "unknown.input",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_link_to_unknown_processor() {
        let mut graph = InternalProcessorLinkGraph::new();
        let proc_0 = graph.add_processor("TestProcessor").id.clone();

        // Link to non-existent processor
        let result = graph.try_add_link_by_address(
            &format!("{}.output", proc_0),
            "unknown.input", // Processor doesn't exist
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_find_link() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        let from_port = format!("{}.output", source);
        let to_port = format!("{}.input", sink);
        let link_id = graph.add_link_by_address(from_port.clone(), to_port.clone());

        // Find by ports
        let found = graph.find_link(&from_port, &to_port);
        assert_eq!(found, Some(link_id.clone()));

        // Find by ID
        let link = graph.find_link_by_id(&link_id);
        assert!(link.is_some());
        assert_eq!(link.unwrap().from_port(), from_port);
    }

    #[test]
    fn test_checksum_deterministic() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();
        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink));

        // Compute checksum twice - should be identical
        let checksum1 = graph.checksum();
        let checksum2 = graph.checksum();

        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_graph() {
        let mut graph = InternalProcessorLinkGraph::new();
        let _proc_0 = graph.add_processor("TestProcessor");

        let checksum1 = graph.checksum();

        // Add another processor
        let _proc_1 = graph.add_processor("TestProcessor");

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
