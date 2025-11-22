# Graph Optimizer Infrastructure (Phase 0)

## Overview

This document describes building the **complete GraphOptimizer infrastructure** while initially producing execution plans identical to current behavior (one thread per processor, current buffer sizes). This allows us to:

1. Build and test the entire graph analysis system
2. Enable graph query/visualization APIs immediately
3. Defer actual optimizations until infrastructure is proven
4. Ship incrementally with zero risk

**Key Principle**: The optimizer is fully implemented but produces "legacy" execution plans that match today's threading model exactly.

## Core Philosophy

**Design Principles**:
1. **Zero Configuration**: Users never think about threads, queues, or performance tuning
2. **Real-time Dynamic Graphs**: Add/remove processors and connections while running - no pause required
3. **Transparent to Users**: All graph analysis happens behind the scenes
4. **Query-First**: Expose graph structure for visualization and debugging before optimizing
5. **Service-Mode First**: Designed for multi-tenant streaming services where processors come and go

**What Gets Built (Phase 0)**:
- Complete GraphOptimizer with petgraph-based graph representation
- Graph analysis and topology detection (sources, sinks, paths)
- Checksum-based caching for repeated patterns
- Execution plan abstraction that supports future optimizations
- Query APIs for web visualization and MCP tools

**What Users See**:
- Same API as today (`add_processor`, `connect`, `start`)
- Same threading behavior (one thread per processor)
- New query APIs to inspect graph structure
- Foundation ready for future optimizations

## Architecture

### Key Components

```
┌─────────────────────────────────────────────────────────────┐
│ StreamRuntime                                               │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ GraphOptimizer                                          │ │
│ │ ┌──────────────┐  ┌──────────────┐  ┌────────────────┐ │ │
│ │ │ petgraph     │  │ Checksum     │  │ Cache          │ │ │
│ │ │ DiGraph      │  │ Computation  │  │ Management     │ │ │
│ │ └──────────────┘  └──────────────┘  └────────────────┘ │ │
│ │                                                         │ │
│ │ ┌─────────────────────────────────────────────────────┐ │ │
│ │ │ ExecutionPlan::Legacy (Phase 0)                     │ │ │
│ │ │ - One thread per processor                          │ │ │
│ │ │ - Current buffer sizes                              │ │ │
│ │ └─────────────────────────────────────────────────────┘ │ │
│ └─────────────────────────────────────────────────────────┘ │
│                                                             │
│ Processors: HashMap<ProcessorId, ProcessorHandle>          │
│ Connections: Vec<Connection>                                │
│ Connection Index: HashMap<ProcessorId, Vec<ConnectionId>>  │
│ Running Threads: HashMap<ProcessorId, JoinHandle>          │
└─────────────────────────────────────────────────────────────┘
```

### Execution Plan Abstraction

The key insight: **separate graph analysis from execution decisions**:

```rust
pub struct GraphOptimizer {
    // petgraph representation for analysis
    graph: DiGraph<ProcessorNode, ConnectionEdge>,

    // Cache: graph checksum → execution plan
    plan_cache: HashMap<GraphChecksum, ExecutionPlan>,

    current_checksum: Option<GraphChecksum>,
}

/// Node in the processor graph
#[derive(Debug, Clone)]
pub struct ProcessorNode {
    pub id: ProcessorId,
    pub processor_type: String,
    pub config_checksum: Option<u64>,
}

/// Edge in the processor graph
#[derive(Debug, Clone)]
pub struct ConnectionEdge {
    pub id: ConnectionId,
    pub from_port: String,
    pub to_port: String,
    pub port_type: PortType,
    pub buffer_capacity: usize,
}

/// Execution plan - how to run the graph
pub enum ExecutionPlan {
    /// Phase 0: Current behavior (one thread per processor)
    Legacy {
        processors: Vec<ProcessorId>,
        connections: Vec<ConnectionId>,
        // Buffer sizes from connections (unchanged)
    },

    // Future phases (not implemented yet):
    // Optimized {
    //     threading: HashMap<ProcessorId, ThreadingDecision>,
    //     buffer_sizes: HashMap<ConnectionId, usize>,
    //     fused_groups: Vec<Vec<ProcessorId>>,
    // },
}
```

## Implementation

### 1. GraphOptimizer Core

**File**: `libs/streamlib/src/core/graph_optimizer.rs`

```rust
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use std::collections::HashMap;

pub struct GraphOptimizer {
    // petgraph representation
    graph: DiGraph<ProcessorNode, ConnectionEdge>,

    // Map processor ID to graph node index
    processor_to_node: HashMap<ProcessorId, NodeIndex>,

    // Cache: graph checksum → execution plan
    plan_cache: HashMap<GraphChecksum, ExecutionPlan>,

    current_checksum: Option<GraphChecksum>,
}

impl GraphOptimizer {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            processor_to_node: HashMap::new(),
            plan_cache: HashMap::new(),
            current_checksum: None,
        }
    }

    /// Add processor to graph (called from StreamRuntime::add_processor)
    pub fn add_processor(&mut self, id: ProcessorId, processor_type: String, config_checksum: Option<u64>) {
        let node = ProcessorNode {
            id: id.clone(),
            processor_type,
            config_checksum,
        };

        let node_idx = self.graph.add_node(node);
        self.processor_to_node.insert(id, node_idx);

        // Invalidate cache
        self.current_checksum = None;
    }

    /// Remove processor from graph (called from StreamRuntime::remove_processor)
    pub fn remove_processor(&mut self, id: &ProcessorId) {
        if let Some(node_idx) = self.processor_to_node.remove(id) {
            self.graph.remove_node(node_idx);

            // Invalidate cache
            self.current_checksum = None;
        }
    }

    /// Add connection to graph (called from StreamRuntime::connect)
    pub fn add_connection(&mut self, connection: &Connection) {
        let from_node = self.processor_to_node.get(&connection.source_processor);
        let to_node = self.processor_to_node.get(&connection.dest_processor);

        if let (Some(&from_idx), Some(&to_idx)) = (from_node, to_node) {
            let edge = ConnectionEdge {
                id: connection.id.clone(),
                from_port: connection.from_port.clone(),
                to_port: connection.to_port.clone(),
                port_type: connection.port_type,
                buffer_capacity: connection.buffer_capacity,
            };

            self.graph.add_edge(from_idx, to_idx, edge);

            // Invalidate cache
            self.current_checksum = None;
        }
    }

    /// Remove connection from graph (called from StreamRuntime::disconnect)
    pub fn remove_connection(&mut self, connection_id: &ConnectionId) {
        // Find and remove edge
        if let Some(edge_idx) = self.graph
            .edge_indices()
            .find(|&e| self.graph[e].id == *connection_id)
        {
            self.graph.remove_edge(edge_idx);

            // Invalidate cache
            self.current_checksum = None;
        }
    }

    /// Analyze graph and produce execution plan
    pub fn optimize(&mut self) -> Result<ExecutionPlan> {
        // Compute checksum for cache lookup
        let checksum = self.compute_checksum();

        // Check cache
        if let Some(cached_plan) = self.plan_cache.get(&checksum) {
            tracing::debug!("✅ Using cached execution plan (checksum: {:x})", checksum.0);
            return Ok(cached_plan.clone());
        }

        // Cache miss - compute fresh plan
        tracing::debug!("🔍 Computing execution plan (checksum: {:x})", checksum.0);

        // Phase 0: Just return legacy plan (one thread per processor)
        let plan = self.compute_legacy_plan();

        // Cache for future
        self.plan_cache.insert(checksum, plan.clone());
        self.current_checksum = Some(checksum);

        Ok(plan)
    }

    /// Phase 0: Generate legacy execution plan (current behavior)
    fn compute_legacy_plan(&self) -> ExecutionPlan {
        // Just list all processors and connections
        let processors: Vec<ProcessorId> = self.graph
            .node_indices()
            .map(|idx| self.graph[idx].id.clone())
            .collect();

        let connections: Vec<ConnectionId> = self.graph
            .edge_indices()
            .map(|idx| self.graph[idx].id.clone())
            .collect();

        ExecutionPlan::Legacy {
            processors,
            connections,
        }
    }
}
```

### 2. Checksum Computation

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphChecksum(pub u64);

impl GraphOptimizer {
    fn compute_checksum(&self) -> GraphChecksum {
        let mut hasher = DefaultHasher::new();

        // Hash all nodes (sorted by ID for determinism)
        let mut nodes: Vec<_> = self.graph.node_indices().collect();
        nodes.sort_by_key(|&idx| &self.graph[idx].id);

        for node_idx in nodes {
            let node = &self.graph[node_idx];
            node.id.hash(&mut hasher);
            node.processor_type.hash(&mut hasher);
            node.config_checksum.hash(&mut hasher);
        }

        // Hash all edges (sorted by connection ID for determinism)
        let mut edges: Vec<_> = self.graph.edge_indices().collect();
        edges.sort_by_key(|&idx| &self.graph[idx].id);

        for edge_idx in edges {
            let edge = &self.graph[edge_idx];
            edge.id.hash(&mut hasher);
            edge.from_port.hash(&mut hasher);
            edge.to_port.hash(&mut hasher);
            // Don't hash buffer_capacity - it may change without affecting structure
        }

        GraphChecksum(hasher.finish())
    }
}
```

### 3. Integration with StreamRuntime

**File**: `libs/streamlib/src/core/runtime.rs`

```rust
pub struct StreamRuntime {
    // ... existing fields ...

    /// Graph optimizer (Phase 0: produces legacy plans)
    optimizer: GraphOptimizer,

    /// Current execution plan
    current_plan: Option<ExecutionPlan>,
}

impl StreamRuntime {
    pub fn new() -> Self {
        Self {
            // ... existing initialization ...
            optimizer: GraphOptimizer::new(),
            current_plan: None,
        }
    }

    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<ProcessorHandle> {
        let proc_id = self.next_id();

        // Compute config checksum for caching
        let config_checksum = compute_config_checksum(&config);

        // Create handle with metadata
        let handle = ProcessorHandle::with_metadata(
            proc_id.clone(),
            std::any::type_name::<P>().to_string(),
            Some(config_checksum),
        );

        // Create processor instance
        let processor = P::from_config(config)?;
        self.processors.insert(proc_id.clone(), Box::new(processor));

        // Update optimizer graph
        self.optimizer.add_processor(
            proc_id.clone(),
            std::any::type_name::<P>().to_string(),
            Some(config_checksum),
        );

        // If runtime is running, reoptimize and apply changes
        if self.is_running {
            self.reoptimize_and_apply()?;
        }

        Ok(handle)
    }

    pub fn remove_processor(&mut self, handle: &ProcessorHandle) -> Result<()> {
        let proc_id = &handle.id;

        // Stop thread if running
        if let Some(join_handle) = self.threads.remove(proc_id) {
            // Signal stop and wait for thread
            self.stop_processor_thread(proc_id)?;
            join_handle.join().ok();
        }

        // Remove from optimizer
        self.optimizer.remove_processor(proc_id);

        // Remove processor
        self.processors.remove(proc_id);

        // Reoptimize if running
        if self.is_running {
            self.reoptimize_and_apply()?;
        }

        Ok(())
    }

    fn connect_at_runtime(
        &mut self,
        from_port: &str,
        to_port: &str,
        port_type: PortType,
    ) -> Result<ConnectionId> {
        // Existing connection logic...
        let connection = Connection::new(
            connection_id.clone(),
            from_port.to_string(),
            to_port.to_string(),
            port_type,
            DEFAULT_BUFFER_CAPACITY,
        );

        self.connections.push(connection.clone());

        // Update connection index (prework)
        self.processor_connections
            .entry(connection.source_processor.clone())
            .or_default()
            .push(connection_id.clone());

        self.processor_connections
            .entry(connection.dest_processor.clone())
            .or_default()
            .push(connection_id.clone());

        // Update optimizer graph
        self.optimizer.add_connection(&connection);

        // Reoptimize if running
        if self.is_running {
            self.reoptimize_and_apply()?;
        }

        Ok(connection_id)
    }

    pub fn disconnect_by_id(&mut self, connection_id: &ConnectionId) -> Result<()> {
        // Existing disconnection logic...

        // Update optimizer
        self.optimizer.remove_connection(connection_id);

        // Reoptimize if running
        if self.is_running {
            self.reoptimize_and_apply()?;
        }

        Ok(())
    }

    /// Reoptimize and apply execution plan (real-time, no pause)
    fn reoptimize_and_apply(&mut self) -> Result<()> {
        // Analyze graph and get execution plan
        let new_plan = self.optimizer.optimize()?;

        // Phase 0: Legacy plan doesn't change anything
        // Future phases will diff plans and apply changes incrementally
        match &new_plan {
            ExecutionPlan::Legacy { .. } => {
                // Already running with legacy threading model
                // Just log the graph structure for debugging
                tracing::debug!("📊 Graph updated: {} processors, {} connections",
                    self.processors.len(),
                    self.connections.len()
                );
            }
        }

        self.current_plan = Some(new_plan);
        Ok(())
    }

    pub fn start(&mut self) -> Result<()> {
        // Analyze graph and get execution plan
        let plan = self.optimizer.optimize()?;

        // Apply execution plan
        match &plan {
            ExecutionPlan::Legacy { processors, connections } => {
                // Phase 0: Spawn one thread per processor (current behavior)
                for proc_id in processors {
                    self.spawn_processor_thread(proc_id)?;
                }

                tracing::info!("▶️  Started {} processors", processors.len());
            }
        }

        self.current_plan = Some(plan);
        self.is_running = true;

        Ok(())
    }
}

/// Compute checksum of config for caching
fn compute_config_checksum<T: Hash>(config: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    config.hash(&mut hasher);
    hasher.finish()
}
```

### 4. Graph Query APIs

Now that we have a petgraph representation, we can expose powerful query APIs:

```rust
impl GraphOptimizer {
    /// Get all processor IDs in topological order (sources first)
    pub fn topological_order(&self) -> Result<Vec<ProcessorId>> {
        use petgraph::algo::toposort;

        let sorted = toposort(&self.graph, None)
            .map_err(|_| StreamError::InvalidGraph("Graph contains cycles".into()))?;

        Ok(sorted.into_iter()
            .map(|idx| self.graph[idx].id.clone())
            .collect())
    }

    /// Find all source processors (no incoming connections)
    pub fn find_sources(&self) -> Vec<ProcessorId> {
        self.graph
            .node_indices()
            .filter(|&idx| self.graph.neighbors_directed(idx, Direction::Incoming).count() == 0)
            .map(|idx| self.graph[idx].id.clone())
            .collect()
    }

    /// Find all sink processors (no outgoing connections)
    pub fn find_sinks(&self) -> Vec<ProcessorId> {
        self.graph
            .node_indices()
            .filter(|&idx| self.graph.neighbors_directed(idx, Direction::Outgoing).count() == 0)
            .map(|idx| self.graph[idx].id.clone())
            .collect()
    }

    /// Get all downstream processors from a given processor
    pub fn get_downstream(&self, proc_id: &ProcessorId) -> Vec<ProcessorId> {
        if let Some(&node_idx) = self.processor_to_node.get(proc_id) {
            self.graph
                .neighbors_directed(node_idx, Direction::Outgoing)
                .map(|idx| self.graph[idx].id.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get all upstream processors from a given processor
    pub fn get_upstream(&self, proc_id: &ProcessorId) -> Vec<ProcessorId> {
        if let Some(&node_idx) = self.processor_to_node.get(proc_id) {
            self.graph
                .neighbors_directed(node_idx, Direction::Incoming)
                .map(|idx| self.graph[idx].id.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Export graph as DOT format for Graphviz visualization
    pub fn to_dot(&self) -> String {
        use petgraph::dot::{Dot, Config};
        format!("{:?}", Dot::with_config(&self.graph, &[Config::EdgeNoLabel]))
    }

    /// Export graph as JSON for web visualization
    pub fn to_json(&self) -> serde_json::Value {
        let nodes: Vec<_> = self.graph
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

        let edges: Vec<_> = self.graph
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
                    "buffer_capacity": edge.buffer_capacity,
                })
            })
            .collect();

        serde_json::json!({
            "nodes": nodes,
            "edges": edges,
        })
    }

    /// Check if graph is valid (no cycles, all connections valid)
    pub fn validate(&self) -> Result<()> {
        use petgraph::algo::is_cyclic_directed;

        if is_cyclic_directed(&self.graph) {
            return Err(StreamError::InvalidGraph("Graph contains cycles".into()));
        }

        // Could add more validation here:
        // - Check all connections reference valid ports
        // - Check port types match
        // - etc.

        Ok(())
    }

    /// Get graph statistics
    pub fn stats(&self) -> GraphStats {
        GraphStats {
            processor_count: self.graph.node_count(),
            connection_count: self.graph.edge_count(),
            source_count: self.find_sources().len(),
            sink_count: self.find_sinks().len(),
            cache_size: self.plan_cache.len(),
            cache_hit_rate: None, // Could track this with counters
        }
    }
}

#[derive(Debug, Clone)]
pub struct GraphStats {
    pub processor_count: usize,
    pub connection_count: usize,
    pub source_count: usize,
    pub sink_count: usize,
    pub cache_size: usize,
    pub cache_hit_rate: Option<f64>,
}
```

### 5. MCP Tools for Graph Visualization

Add new MCP tools that expose the graph query APIs:

```rust
// libs/streamlib/src/mcp/tools.rs

fn register_graph_tools(server: &mut Server) {
    // Export graph as DOT for Graphviz
    server.add_tool(
        "export_graph_dot",
        "Export processor graph as DOT format for Graphviz visualization",
        |_args, runtime| {
            let runtime = runtime.lock().unwrap();
            let dot = runtime.optimizer.to_dot();
            Ok(json!({ "dot": dot }))
        },
    );

    // Export graph as JSON for web visualization
    server.add_tool(
        "export_graph_json",
        "Export processor graph as JSON for web visualization",
        |_args, runtime| {
            let runtime = runtime.lock().unwrap();
            let json = runtime.optimizer.to_json();
            Ok(json)
        },
    );

    // Get graph statistics
    server.add_tool(
        "get_graph_stats",
        "Get statistics about the processor graph",
        |_args, runtime| {
            let runtime = runtime.lock().unwrap();
            let stats = runtime.optimizer.stats();
            Ok(serde_json::to_value(stats).unwrap())
        },
    );

    // Find sources/sinks
    server.add_tool(
        "find_sources",
        "Find all source processors (no inputs)",
        |_args, runtime| {
            let runtime = runtime.lock().unwrap();
            let sources = runtime.optimizer.find_sources();
            Ok(json!({ "sources": sources }))
        },
    );

    server.add_tool(
        "find_sinks",
        "Find all sink processors (no outputs)",
        |_args, runtime| {
            let runtime = runtime.lock().unwrap();
            let sinks = runtime.optimizer.find_sinks();
            Ok(json!({ "sinks": sinks }))
        },
    );

    // Get downstream/upstream processors
    server.add_tool(
        "get_downstream",
        "Get all processors downstream from a given processor",
        |args, runtime| {
            let proc_id = args["processor_id"].as_str()
                .ok_or("Missing processor_id")?;
            let runtime = runtime.lock().unwrap();
            let downstream = runtime.optimizer.get_downstream(proc_id);
            Ok(json!({ "downstream": downstream }))
        },
    );

    server.add_tool(
        "get_upstream",
        "Get all processors upstream from a given processor",
        |args, runtime| {
            let proc_id = args["processor_id"].as_str()
                .ok_or("Missing processor_id")?;
            let runtime = runtime.lock().unwrap();
            let upstream = runtime.optimizer.get_upstream(proc_id);
            Ok(json!({ "upstream": upstream }))
        },
    );
}
```

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_graph() {
        let mut optimizer = GraphOptimizer::new();
        let plan = optimizer.optimize().unwrap();

        match plan {
            ExecutionPlan::Legacy { processors, connections } => {
                assert_eq!(processors.len(), 0);
                assert_eq!(connections.len(), 0);
            }
        }
    }

    #[test]
    fn test_add_remove_processor() {
        let mut optimizer = GraphOptimizer::new();

        optimizer.add_processor("proc1".into(), "TestProcessor".into(), None);
        assert_eq!(optimizer.graph.node_count(), 1);

        optimizer.remove_processor(&"proc1".into());
        assert_eq!(optimizer.graph.node_count(), 0);
    }

    #[test]
    fn test_checksum_determinism() {
        let mut opt1 = GraphOptimizer::new();
        let mut opt2 = GraphOptimizer::new();

        // Add same processors/connections in same order
        opt1.add_processor("a".into(), "TypeA".into(), Some(123));
        opt1.add_processor("b".into(), "TypeB".into(), Some(456));

        opt2.add_processor("a".into(), "TypeA".into(), Some(123));
        opt2.add_processor("b".into(), "TypeB".into(), Some(456));

        assert_eq!(opt1.compute_checksum(), opt2.compute_checksum());
    }

    #[test]
    fn test_find_sources_sinks() {
        let mut optimizer = GraphOptimizer::new();

        // Graph: source → middle → sink
        optimizer.add_processor("source".into(), "Source".into(), None);
        optimizer.add_processor("middle".into(), "Middle".into(), None);
        optimizer.add_processor("sink".into(), "Sink".into(), None);

        let conn1 = Connection::new(
            "c1".into(),
            "source.out".into(),
            "middle.in".into(),
            PortType::Video,
            10,
        );
        let conn2 = Connection::new(
            "c2".into(),
            "middle.out".into(),
            "sink.in".into(),
            PortType::Video,
            10,
        );

        optimizer.add_connection(&conn1);
        optimizer.add_connection(&conn2);

        let sources = optimizer.find_sources();
        let sinks = optimizer.find_sinks();

        assert_eq!(sources, vec!["source".to_string()]);
        assert_eq!(sinks, vec!["sink".to_string()]);
    }

    #[test]
    fn test_cache_hit() {
        let mut optimizer = GraphOptimizer::new();

        optimizer.add_processor("proc1".into(), "Test".into(), None);

        // First call - cache miss
        let plan1 = optimizer.optimize().unwrap();
        assert_eq!(optimizer.plan_cache.len(), 1);

        // Second call - cache hit (same graph)
        let plan2 = optimizer.optimize().unwrap();
        assert_eq!(optimizer.plan_cache.len(), 1);
    }

    #[test]
    fn test_to_json() {
        let mut optimizer = GraphOptimizer::new();

        optimizer.add_processor("proc1".into(), "TestProcessor".into(), Some(123));

        let json = optimizer.to_json();

        assert_eq!(json["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(json["nodes"][0]["id"], "proc1");
        assert_eq!(json["nodes"][0]["type"], "TestProcessor");
    }
}
```

### Integration Tests

```rust
#[test]
fn test_runtime_with_optimizer() {
    let mut runtime = StreamRuntime::new();

    // Add processors
    let camera = runtime.add_processor_with_config::<CameraProcessor>(CameraConfig {
        device_id: None,
    }).unwrap();

    let display = runtime.add_processor_with_config::<DisplayProcessor>(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("Test".to_string()),
        scaling_mode: ScalingMode::Fit,
    }).unwrap();

    // Connect
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        display.input_port::<VideoFrame>("video"),
    ).unwrap();

    // Start - should produce legacy execution plan
    runtime.start().unwrap();

    // Verify: 2 threads spawned (one per processor)
    assert_eq!(runtime.threads.len(), 2);

    // Query graph
    let sources = runtime.optimizer.find_sources();
    assert_eq!(sources.len(), 1);

    let sinks = runtime.optimizer.find_sinks();
    assert_eq!(sinks.len(), 1);
}

#[test]
fn test_dynamic_graph_changes() {
    let mut runtime = StreamRuntime::new();
    runtime.start().unwrap();

    // Add processor while running
    let camera = runtime.add_processor_with_config::<CameraProcessor>(/* ... */).unwrap();

    // Verify: thread started immediately
    assert_eq!(runtime.threads.len(), 1);

    // Remove processor while running
    runtime.remove_processor(&camera).unwrap();

    // Verify: thread stopped
    assert_eq!(runtime.threads.len(), 0);
}
```

## Performance Targets

**Phase 0 Targets**:
- Graph update (add/remove processor): <50μs (just petgraph operations)
- Optimization (uncached): <100μs (simple legacy plan generation)
- Optimization (cached): <10μs (hash lookup)
- Zero runtime performance impact (same threading as today)

**Memory Overhead**:
- petgraph: ~100 bytes per processor + ~50 bytes per connection
- Cache: ~200 bytes per unique graph topology
- For 1000-processor service: ~150KB total overhead

## Deliverables (Phase 0)

When Phase 0 is complete, we have:

✅ **Full GraphOptimizer infrastructure**
- petgraph-based graph representation
- Checksum-based caching
- Execution plan abstraction
- Real-time graph updates (add/remove while running)

✅ **Query APIs for visualization**
- Export as DOT (Graphviz)
- Export as JSON (web apps)
- Find sources/sinks
- Traverse upstream/downstream
- Get graph statistics

✅ **Zero behavior change**
- Same threading model (one thread per processor)
- Same buffer sizes
- All existing examples work unchanged

✅ **Foundation for future optimizations**
- Ready to swap `ExecutionPlan::Legacy` → `ExecutionPlan::Optimized`
- Graph analysis code reusable for all optimization strategies
- Cache infrastructure ready for complex plans

## Next Steps

After Phase 0 is complete and proven stable:

1. **Ship Phase 0** - Get graph query APIs into users' hands
2. **Build visualizations** - Web dashboard showing graph structure
3. **Start Phase 1** - Implement first optimization strategy (see `graph_optimizer_strategies.md`)

The key is: we can ship the query/visualization features immediately while deferring the risky optimization work.
