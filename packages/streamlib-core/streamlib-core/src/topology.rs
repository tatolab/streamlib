//! Topology analysis for stream graphs
//!
//! This module provides tools to analyze the connection topology of a StreamRuntime
//! for visualization, debugging, and validation purposes. It's separate from the
//! runtime execution to avoid performance overhead.
//!
//! The topology analyzer walks the flat handler registry and builds a cached
//! graph representation that can be exported to various formats (JSON, GraphViz, etc.).

use std::collections::HashMap;

/// Connection topology snapshot
///
/// Represents the current state of handler connections at a point in time.
/// This is built on-demand by analyzing a StreamRuntime.
#[derive(Debug, Clone)]
pub struct ConnectionTopology {
    /// All handlers in the runtime
    pub nodes: HashMap<String, NodeInfo>,
    /// All connections between handlers
    pub edges: Vec<Edge>,
}

/// Information about a handler node
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Unique handler identifier
    pub handler_id: String,
    /// Type of handler (e.g., "VideoCapture", "Shader", "Display")
    pub handler_type: String,
    /// Input ports
    pub inputs: Vec<PortInfo>,
    /// Output ports
    pub outputs: Vec<PortInfo>,
}

/// Information about a port
#[derive(Debug, Clone)]
pub struct PortInfo {
    /// Port name (e.g., "video", "audio")
    pub port_name: String,
    /// Port type/format (e.g., "video", "audio", "texture")
    pub port_type: String,
}

/// Connection between two ports
#[derive(Debug, Clone)]
pub struct Edge {
    /// Source handler ID
    pub from_handler: String,
    /// Source port name
    pub from_port: String,
    /// Destination handler ID
    pub to_handler: String,
    /// Destination port name
    pub to_port: String,
}

impl ConnectionTopology {
    /// Create an empty topology
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
        }
    }

    /// Print the graph in a human-readable format
    pub fn print(&self) {
        println!("=== Stream Graph ===");
        println!("\nNodes ({}):", self.nodes.len());
        for (id, node) in &self.nodes {
            println!("  - {} ({})", id, node.handler_type);
            if !node.inputs.is_empty() {
                println!("    Inputs: {:?}", node.inputs.iter().map(|p| &p.port_name).collect::<Vec<_>>());
            }
            if !node.outputs.is_empty() {
                println!("    Outputs: {:?}", node.outputs.iter().map(|p| &p.port_name).collect::<Vec<_>>());
            }
        }

        println!("\nConnections ({}):", self.edges.len());
        for edge in &self.edges {
            println!("  {} [{}] -> {} [{}]",
                edge.from_handler, edge.from_port,
                edge.to_handler, edge.to_port
            );
        }
    }

    /// Export topology as GraphViz DOT format
    ///
    /// Returns a string that can be rendered with graphviz tools.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let topology = TopologyAnalyzer::analyze(&runtime);
    /// let dot = topology.to_graphviz();
    /// std::fs::write("graph.dot", dot).unwrap();
    /// // Then: dot -Tpng graph.dot -o graph.png
    /// ```
    pub fn to_graphviz(&self) -> String {
        let mut dot = String::from("digraph StreamGraph {\n");
        dot.push_str("  rankdir=LR;\n");
        dot.push_str("  node [shape=box];\n\n");

        // Add nodes
        for (id, node) in &self.nodes {
            dot.push_str(&format!("  \"{}\" [label=\"{}\\n({})\"];\n",
                id, id, node.handler_type));
        }

        dot.push_str("\n");

        // Add edges
        for edge in &self.edges {
            dot.push_str(&format!("  \"{}\" -> \"{}\" [label=\"{}→{}\"];\n",
                edge.from_handler, edge.to_handler,
                edge.from_port, edge.to_port
            ));
        }

        dot.push_str("}\n");
        dot
    }
}

impl Default for ConnectionTopology {
    fn default() -> Self {
        Self::new()
    }
}

/// Topology analyzer
///
/// Provides methods to analyze a StreamRuntime and build topology snapshots.
pub struct TopologyAnalyzer;

impl TopologyAnalyzer {
    /// Analyze a StreamRuntime and build topology snapshot
    ///
    /// This method will be implemented once we have the actual StreamRuntime
    /// with handler introspection capabilities.
    ///
    /// # Example (future usage)
    ///
    /// ```ignore
    /// use streamlib_core::TopologyAnalyzer;
    ///
    /// // Assuming you have a concrete StreamRuntime implementation
    /// let runtime: Box<dyn StreamRuntime> = ...;
    /// // ... add handlers, connect ports ...
    ///
    /// let topology = TopologyAnalyzer::analyze(&*runtime);
    /// println!("Found {} handlers", topology.nodes.len());
    /// println!("Found {} connections", topology.edges.len());
    /// ```
    pub fn analyze(_runtime: &dyn crate::runtime::StreamRuntime) -> ConnectionTopology {
        // TODO: Implement once StreamRuntime has introspection methods
        // This would:
        // 1. Walk through runtime.handlers
        // 2. For each handler, introspect its input/output ports
        // 3. For each input port, check if it has a connected output
        // 4. Build NodeInfo and Edge entries

        ConnectionTopology::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_topology() {
        let topology = ConnectionTopology::new();
        assert_eq!(topology.nodes.len(), 0);
        assert_eq!(topology.edges.len(), 0);
    }

    #[test]
    fn test_print_simple_graph() {
        let mut topology = ConnectionTopology::new();

        // Add nodes
        topology.nodes.insert(
            "camera".to_string(),
            NodeInfo {
                handler_id: "camera".to_string(),
                handler_type: "VideoCapture".to_string(),
                inputs: vec![],
                outputs: vec![PortInfo {
                    port_name: "video".to_string(),
                    port_type: "video".to_string(),
                }],
            },
        );

        topology.nodes.insert(
            "filter".to_string(),
            NodeInfo {
                handler_id: "filter".to_string(),
                handler_type: "Shader".to_string(),
                inputs: vec![PortInfo {
                    port_name: "input".to_string(),
                    port_type: "video".to_string(),
                }],
                outputs: vec![PortInfo {
                    port_name: "output".to_string(),
                    port_type: "video".to_string(),
                }],
            },
        );

        // Connect them
        topology.edges.push(Edge {
            from_handler: "camera".to_string(),
            from_port: "video".to_string(),
            to_handler: "filter".to_string(),
            to_port: "input".to_string(),
        });

        // Just verify it doesn't panic
        topology.print();
    }

    #[test]
    fn test_to_graphviz() {
        let mut topology = ConnectionTopology::new();

        // Add simple chain: A -> B
        topology.nodes.insert(
            "A".to_string(),
            NodeInfo {
                handler_id: "A".to_string(),
                handler_type: "Source".to_string(),
                inputs: vec![],
                outputs: vec![],
            },
        );

        topology.nodes.insert(
            "B".to_string(),
            NodeInfo {
                handler_id: "B".to_string(),
                handler_type: "Sink".to_string(),
                inputs: vec![],
                outputs: vec![],
            },
        );

        topology.edges.push(Edge {
            from_handler: "A".to_string(),
            from_port: "out".to_string(),
            to_handler: "B".to_string(),
            to_port: "in".to_string(),
        });

        let dot = topology.to_graphviz();

        // Verify it contains expected GraphViz syntax
        assert!(dot.contains("digraph StreamGraph"));
        assert!(dot.contains("\"A\""));
        assert!(dot.contains("\"B\""));
        assert!(dot.contains("->"));
    }
}
