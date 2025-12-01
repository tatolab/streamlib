use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use super::running::{RunningProcessor, WiredLink};
use crate::core::graph::{Graph, GraphChecksum, ProcessorId};
use crate::core::link_channel::LinkId;

/// Metadata about when and how the execution graph was compiled
#[derive(Debug, Clone)]
pub(crate) struct CompilationMetadata {
    /// When the graph was compiled
    pub compiled_at: Instant,
    /// Checksum of the source Graph at compile time
    pub source_checksum: GraphChecksum,
}

impl CompilationMetadata {
    /// Create new compilation metadata
    pub fn new(source_checksum: GraphChecksum) -> Self {
        Self {
            compiled_at: Instant::now(),
            source_checksum,
        }
    }

    /// Get time elapsed since compilation
    pub fn elapsed(&self) -> std::time::Duration {
        self.compiled_at.elapsed()
    }
}

/// Manual Serialize for CompilationMetadata
///
/// Serializes elapsed time (as millis) since Instant can't be serialized directly
impl Serialize for CompilationMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("CompilationMetadata", 2)?;
        s.serialize_field("elapsed_ms", &self.elapsed().as_millis())?;
        s.serialize_field("source_checksum", &self.source_checksum.0)?;
        s.end()
    }
}

/// Runtime state for a processor (the "extension" part of RunningProcessor)
///
/// This is stored separately from ProcessorNode to allow the Graph topology
/// to remain unchanged while we track runtime state.
pub(crate) type ProcessorRuntimeState = RunningProcessor;

/// Runtime state for a link (the "extension" part of WiredLink)
pub(crate) type LinkRuntimeState = WiredLink;

/// The execution graph - extends Graph with runtime state
///
/// Implements `Deref<Target = Graph>` so all Graph methods work transparently.
/// Runtime state (threads, channels, ring buffers) is stored in parallel HashMaps
/// indexed by the same IDs used in the Graph.
///
/// This follows the same pattern as:
/// - `RunningProcessor` extends `ProcessorNode`
/// - `WiredLink` extends `Link`
pub(crate) struct ExecutionGraph {
    /// The underlying graph (topology) - we extend this with runtime state
    graph: Arc<RwLock<Graph>>,
    /// Compilation metadata (when compiled, source checksum)
    pub metadata: CompilationMetadata,
    /// Runtime state for each processor (keyed by processor ID)
    processor_runtime: HashMap<ProcessorId, ProcessorRuntimeState>,
    /// Runtime state for each link (keyed by link ID)
    link_runtime: HashMap<LinkId, LinkRuntimeState>,
}

impl Deref for ExecutionGraph {
    type Target = Arc<RwLock<Graph>>;

    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

impl ExecutionGraph {
    /// Create a new execution graph from a Graph reference
    ///
    /// The ExecutionGraph holds a reference to the Graph (shared ownership)
    /// and adds runtime state on top.
    pub fn new(graph: Arc<RwLock<Graph>>, metadata: CompilationMetadata) -> Self {
        Self {
            graph,
            metadata,
            processor_runtime: HashMap::new(),
            link_runtime: HashMap::new(),
        }
    }

    /// Get the underlying graph reference
    #[allow(dead_code)]
    pub fn graph(&self) -> &Arc<RwLock<Graph>> {
        &self.graph
    }

    // =========================================================================
    // Processor Runtime State
    // =========================================================================

    /// Get runtime state for a processor
    pub fn get_processor_runtime(&self, id: &ProcessorId) -> Option<&ProcessorRuntimeState> {
        self.processor_runtime.get(id)
    }

    /// Get mutable runtime state for a processor
    pub fn get_processor_runtime_mut(
        &mut self,
        id: &ProcessorId,
    ) -> Option<&mut ProcessorRuntimeState> {
        self.processor_runtime.get_mut(id)
    }

    /// Insert runtime state for a processor
    pub fn insert_processor_runtime(&mut self, id: ProcessorId, state: ProcessorRuntimeState) {
        self.processor_runtime.insert(id, state);
    }

    /// Remove runtime state for a processor
    pub fn remove_processor_runtime(&mut self, id: &ProcessorId) -> Option<ProcessorRuntimeState> {
        self.processor_runtime.remove(id)
    }

    /// Iterate over all processor runtime states
    pub fn iter_processor_runtime(
        &self,
    ) -> impl Iterator<Item = (&ProcessorId, &ProcessorRuntimeState)> {
        self.processor_runtime.iter()
    }

    /// Get all processor IDs that have runtime state
    pub fn processor_ids(&self) -> impl Iterator<Item = &ProcessorId> {
        self.processor_runtime.keys()
    }

    /// Get the number of processors with runtime state
    pub fn processor_count(&self) -> usize {
        self.processor_runtime.len()
    }

    // =========================================================================
    // Link Runtime State
    // =========================================================================

    /// Get runtime state for a link
    pub fn get_link_runtime(&self, id: &LinkId) -> Option<&LinkRuntimeState> {
        self.link_runtime.get(id)
    }

    /// Get mutable runtime state for a link
    #[allow(dead_code)]
    pub fn get_link_runtime_mut(&mut self, id: &LinkId) -> Option<&mut LinkRuntimeState> {
        self.link_runtime.get_mut(id)
    }

    /// Insert runtime state for a link
    pub fn insert_link_runtime(&mut self, id: LinkId, state: LinkRuntimeState) {
        self.link_runtime.insert(id, state);
    }

    /// Remove runtime state for a link
    pub fn remove_link_runtime(&mut self, id: &LinkId) -> Option<LinkRuntimeState> {
        self.link_runtime.remove(id)
    }

    /// Iterate over all link runtime states
    pub fn iter_link_runtime(&self) -> impl Iterator<Item = (&LinkId, &LinkRuntimeState)> {
        self.link_runtime.iter()
    }

    /// Get the number of links with runtime state
    pub fn link_count(&self) -> usize {
        self.link_runtime.len()
    }

    // =========================================================================
    // Bulk Operations
    // =========================================================================

    /// Clear all runtime state (processors and links)
    ///
    /// Note: This does NOT modify the underlying Graph - only clears runtime state.
    pub fn clear_runtime_state(&mut self) {
        self.processor_runtime.clear();
        self.link_runtime.clear();
    }

    /// Check if the graph has changed since compilation
    ///
    /// Compares the current graph checksum against the checksum at compile time.
    pub fn needs_recompile(&self) -> bool {
        let current_checksum = self.graph.read().checksum();
        current_checksum != self.metadata.source_checksum
    }
}

/// Manual Serialize implementation for ExecutionGraph
///
/// Serializes the complete runtime state for debugging/testing:
/// - graph: The underlying Graph topology (nodes, links, ports)
/// - metadata: Compilation metadata (elapsed time, checksum)
/// - processors: Map of processor ID → RunningProcessor state
/// - links: Map of link ID → WiredLink state
/// - needs_recompile: Whether the source graph has changed
///
/// This provides a complete snapshot of the execution state that can be
/// used for testing assertions, debugging, or visualization.
impl Serialize for ExecutionGraph {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("ExecutionGraph", 5)?;

        // Serialize the underlying graph (requires read lock)
        s.serialize_field("graph", &*self.graph.read())?;

        // Serialize compilation metadata
        s.serialize_field("metadata", &self.metadata)?;

        // Serialize processor runtime state
        s.serialize_field("processors", &self.processor_runtime)?;

        // Serialize link runtime state
        s.serialize_field("links", &self.link_runtime)?;

        // Include recompile status as a convenience
        s.serialize_field("needs_recompile", &self.needs_recompile())?;

        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compilation_metadata() {
        let checksum = GraphChecksum(12345);
        let metadata = CompilationMetadata::new(checksum);

        assert_eq!(metadata.source_checksum, checksum);
        // Elapsed time should be very small
        assert!(metadata.elapsed().as_millis() < 100);
    }

    #[test]
    fn test_execution_graph_wraps_graph() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let checksum = graph.read().checksum();
        let metadata = CompilationMetadata::new(checksum);
        let exec_graph = ExecutionGraph::new(Arc::clone(&graph), metadata);

        // ExecutionGraph derefs to Arc<RwLock<Graph>>
        assert_eq!(exec_graph.read().processor_count(), 0);

        // Runtime state is separate
        assert_eq!(exec_graph.processor_count(), 0);
        assert_eq!(exec_graph.link_count(), 0);
    }

    #[test]
    fn test_needs_recompile() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let checksum = graph.read().checksum();
        let metadata = CompilationMetadata::new(checksum);
        let exec_graph = ExecutionGraph::new(Arc::clone(&graph), metadata);

        // Initially doesn't need recompile
        assert!(!exec_graph.needs_recompile());

        // Modify the graph
        graph
            .write()
            .add_processor("test".into(), "TestProcessor".into(), 0);

        // Now needs recompile because checksum changed
        assert!(exec_graph.needs_recompile());
    }

    #[test]
    fn test_execution_graph_serialization() {
        let graph = Arc::new(RwLock::new(Graph::new()));

        // Add some processors to the graph
        {
            let mut g = graph.write();
            g.add_processor("source".into(), "SourceProcessor".into(), 0);
            g.add_processor("sink".into(), "SinkProcessor".into(), 0);
            g.add_link_by_address("source.output".into(), "sink.input".into());
        }

        let checksum = graph.read().checksum();
        let metadata = CompilationMetadata::new(checksum);
        let exec_graph = ExecutionGraph::new(Arc::clone(&graph), metadata);

        // Serialize to JSON
        let json = serde_json::to_value(&exec_graph).expect("serialization should succeed");

        // Verify structure
        assert!(json.get("graph").is_some(), "should have graph field");
        assert!(json.get("metadata").is_some(), "should have metadata field");
        assert!(
            json.get("processors").is_some(),
            "should have processors field"
        );
        assert!(json.get("links").is_some(), "should have links field");
        assert!(
            json.get("needs_recompile").is_some(),
            "should have needs_recompile field"
        );

        // Verify graph content
        let graph_json = &json["graph"];
        assert_eq!(graph_json["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(graph_json["links"].as_array().unwrap().len(), 1);

        // Verify metadata
        let metadata_json = &json["metadata"];
        assert!(metadata_json["elapsed_ms"].as_u64().is_some());
        assert!(metadata_json["source_checksum"].as_u64().is_some());

        // needs_recompile should be false (graph unchanged)
        assert_eq!(json["needs_recompile"], false);
    }
}
