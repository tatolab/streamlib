//! Stream graph management
//!
//! Manages connections between stream handlers and tracks data flow.

use crate::runtime::{InputPort, OutputPort, StreamHandler};
use std::collections::HashMap;

pub struct StreamGraph {
    handlers: HashMap<String, Box<dyn StreamHandler>>,
    connections: Vec<(OutputPort, InputPort)>,
}

impl StreamGraph {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            connections: Vec::new(),
        }
    }

    pub fn add_handler(&mut self, id: String, handler: Box<dyn StreamHandler>) {
        self.handlers.insert(id, handler);
    }

    pub fn connect(&mut self, output: OutputPort, input: InputPort) {
        self.connections.push((output, input));
    }
}

impl Default for StreamGraph {
    fn default() -> Self {
        Self::new()
    }
}
