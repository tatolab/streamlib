
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ConnectionTopology {
    pub nodes: HashMap<String, NodeInfo>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub handler_id: String,
    pub handler_type: String,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

#[derive(Debug, Clone)]
pub struct PortInfo {
    pub port_name: String,
    pub port_type: String,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from_handler: String,
    pub from_port: String,
    pub to_handler: String,
    pub to_port: String,
}

impl ConnectionTopology {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
        }
    }

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

    pub fn to_graphviz(&self) -> String {
        let mut dot = String::from("digraph StreamGraph {\n");
        dot.push_str("  rankdir=LR;\n");
        dot.push_str("  node [shape=box];\n\n");

        for (id, node) in &self.nodes {
            dot.push_str(&format!("  \"{}\" [label=\"{}\\n({})\"];\n",
                id, id, node.handler_type));
        }

        dot.push('\n');

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

pub struct TopologyAnalyzer;

impl TopologyAnalyzer {
    pub fn analyze(_runtime: &crate::core::runtime::StreamRuntime) -> ConnectionTopology {
        // TODO: Implement once StreamRuntime has introspection methods

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

        topology.edges.push(Edge {
            from_handler: "camera".to_string(),
            from_port: "video".to_string(),
            to_handler: "filter".to_string(),
            to_port: "input".to_string(),
        });

        topology.print();
    }

    #[test]
    fn test_to_graphviz() {
        let mut topology = ConnectionTopology::new();

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

        assert!(dot.contains("digraph StreamGraph"));
        assert!(dot.contains("\"A\""));
        assert!(dot.contains("\"B\""));
        assert!(dot.contains("->"));
    }
}
