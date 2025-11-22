// Integration tests for GraphOptimizer with StreamRuntime
//
// These tests verify that the GraphOptimizer correctly tracks the processor graph
// and that Phase 0 (Legacy execution plans) produces zero behavior change.

use std::sync::Arc;
use streamlib::core::{ExecutionPlan, Result, StreamRuntime, VideoFrame};

/// Dummy processor for testing
#[derive(streamlib_macros::StreamProcessor)]
struct DummyProcessor {
    #[input]
    input: streamlib::core::StreamInput<VideoFrame>,
    #[output]
    output: Arc<streamlib::core::StreamOutput<VideoFrame>>,
}

impl DummyProcessor {
    fn setup(&mut self, _ctx: &streamlib::core::RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            self.output.write(frame);
        }
        Ok(())
    }
}

#[test]
fn test_optimizer_tracks_processors() {
    let mut runtime = StreamRuntime::new();

    // Initially empty
    assert_eq!(runtime.graph_optimizer().stats().processor_count, 0);

    // Add processors
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Optimizer should track them
    assert_eq!(runtime.graph_optimizer().stats().processor_count, 2);

    // Remove processor
    runtime.remove_processor(p1.id()).unwrap();

    // Optimizer should update
    assert_eq!(runtime.graph_optimizer().stats().processor_count, 1);

    // Remove second processor
    runtime.remove_processor(p2.id()).unwrap();
    assert_eq!(runtime.graph_optimizer().stats().processor_count, 0);
}

#[test]
fn test_optimizer_tracks_connections() {
    let mut runtime = StreamRuntime::new();

    // Add processors
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    // Connect them
    let conn = runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    // Optimizer should track connection
    assert_eq!(runtime.graph_optimizer().stats().connection_count, 1);

    // Disconnect
    runtime.disconnect_by_id(&conn).unwrap();

    // Optimizer should update
    assert_eq!(runtime.graph_optimizer().stats().connection_count, 0);
}

#[test]
fn test_legacy_execution_plan() {
    let mut runtime = StreamRuntime::new();

    // Add processors
    let _p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let _p2 = runtime.add_processor::<DummyProcessor>().unwrap();
    let _p3 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Generate execution plan
    let plan = runtime.graph_optimizer_mut().optimize();

    // Should be Legacy plan
    match plan {
        ExecutionPlan::Legacy {
            processors,
            connections,
        } => {
            assert_eq!(processors.len(), 3);
            assert_eq!(connections.len(), 0);
        }
    }
}

#[test]
fn test_find_sources_sinks() {
    let mut runtime = StreamRuntime::new();

    // Create pipeline: p1 -> p2 -> p3
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p3 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p2.id()),
            &format!("{}.input", p3.id()),
        )
        .unwrap();

    // Find sources and sinks
    let sources = runtime.graph_optimizer().find_sources();
    let sinks = runtime.graph_optimizer().find_sinks();

    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0], p1.id().to_string());

    assert_eq!(sinks.len(), 1);
    assert_eq!(sinks[0], p3.id().to_string());
}

#[test]
fn test_topological_order() {
    let mut runtime = StreamRuntime::new();

    // Create pipeline: p1 -> p2 -> p3
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p3 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p2.id()),
            &format!("{}.input", p3.id()),
        )
        .unwrap();

    // Get topological order
    let order = runtime.graph_optimizer().topological_order();

    assert_eq!(order.len(), 3);

    // p1 must come before p2, p2 before p3
    let p1_idx = order
        .iter()
        .position(|id| id == p1.id())
        .expect("p1 not found");
    let p2_idx = order
        .iter()
        .position(|id| id == p2.id())
        .expect("p2 not found");
    let p3_idx = order
        .iter()
        .position(|id| id == p3.id())
        .expect("p3 not found");

    assert!(p1_idx < p2_idx, "p1 should come before p2");
    assert!(p2_idx < p3_idx, "p2 should come before p3");
}

#[test]
fn test_upstream_downstream_traversal() {
    let mut runtime = StreamRuntime::new();

    // Create pipeline: p1 -> p2 -> p3
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p3 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p2.id()),
            &format!("{}.input", p3.id()),
        )
        .unwrap();

    // Find upstream of p3
    let upstream = runtime
        .graph_optimizer()
        .find_upstream(&p3.id().to_string());
    assert_eq!(upstream.len(), 2);
    assert!(upstream.contains(&p1.id().to_string()));
    assert!(upstream.contains(&p2.id().to_string()));

    // Find downstream of p1
    let downstream = runtime
        .graph_optimizer()
        .find_downstream(&p1.id().to_string());
    assert_eq!(downstream.len(), 2);
    assert!(downstream.contains(&p2.id().to_string()));
    assert!(downstream.contains(&p3.id().to_string()));
}

#[test]
fn test_plan_caching() {
    let mut runtime = StreamRuntime::new();

    // Add processors
    let _p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let _p2 = runtime.add_processor::<DummyProcessor>().unwrap();

    // First optimization - cache miss
    runtime.graph_optimizer_mut().optimize();
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 1);
    assert_eq!(runtime.graph_optimizer().stats().cache_hits, 0);

    // Second optimization - cache hit (same graph structure)
    runtime.graph_optimizer_mut().optimize();
    assert_eq!(runtime.graph_optimizer().stats().cache_hits, 1);
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 1);

    // Add processor - invalidates cache
    let p3 = runtime.add_processor::<DummyProcessor>().unwrap();
    eprintln!(
        "After add p3: processor_count = {}",
        runtime.graph_optimizer().stats().processor_count
    );

    // Next optimization - cache miss (graph changed)
    runtime.graph_optimizer_mut().optimize();
    eprintln!(
        "After optimize (with p3): cache_misses = {}",
        runtime.graph_optimizer().stats().cache_misses
    );
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 2);

    // Remove processor - back to same structure as before (2 processors)
    runtime.remove_processor(p3.id()).unwrap();
    eprintln!(
        "After remove p3: processor_count = {}",
        runtime.graph_optimizer().stats().processor_count
    );

    // This should be a cache HIT since we're back to the same structure as after step 1
    // (2 processors, no connections - same checksum)
    runtime.graph_optimizer_mut().optimize();
    eprintln!(
        "After optimize (removed p3): cache_hits = {}, cache_misses = {}",
        runtime.graph_optimizer().stats().cache_hits,
        runtime.graph_optimizer().stats().cache_misses
    );
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 2); // Still 2, got cache hit
    assert_eq!(runtime.graph_optimizer().stats().cache_hits, 2); // Now 2 hits (step 2 and this one)
}

#[test]
fn test_json_export() {
    let mut runtime = StreamRuntime::new();

    // Create simple pipeline
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    // Export as JSON
    let json = runtime.graph_optimizer().to_json().unwrap();

    // Verify JSON structure
    assert!(json.contains("processors"));
    assert!(json.contains("connections"));
    // Note: processor type shows as trait object type, not concrete type
    assert!(json.contains("DynStreamElement"));
    assert!(json.contains(p1.id()));
    assert!(json.contains(p2.id()));
}

#[test]
fn test_dot_export() {
    let mut runtime = StreamRuntime::new();

    // Create simple pipeline
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    // Export as DOT
    let dot = runtime.graph_optimizer().to_dot();

    // Verify DOT structure
    assert!(dot.contains("digraph"));
    assert!(dot.contains(p1.id()));
    assert!(dot.contains(p2.id()));
    assert!(dot.contains("->"));
}

#[test]
fn test_diamond_graph() {
    let mut runtime = StreamRuntime::new();

    // Create fork: p1 -> p2 -> p4
    //                 \-> p3 -> p5
    // (Two parallel branches, not diamond, since DummyProcessor has single input)
    let p1 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p2 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p3 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p4 = runtime.add_processor::<DummyProcessor>().unwrap();
    let p5 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Start runtime to initialize processor references
    runtime.start().unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p2.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p1.id()),
            &format!("{}.input", p3.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p2.id()),
            &format!("{}.input", p4.id()),
        )
        .unwrap();

    runtime
        .connect_at_runtime(
            &format!("{}.output", p3.id()),
            &format!("{}.input", p5.id()),
        )
        .unwrap();

    // Verify graph structure
    assert_eq!(runtime.graph_optimizer().stats().processor_count, 5);
    assert_eq!(runtime.graph_optimizer().stats().connection_count, 4);

    // p1 is the only source
    let sources = runtime.graph_optimizer().find_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0], p1.id().to_string());

    // p4 and p5 are sinks
    let sinks = runtime.graph_optimizer().find_sinks();
    assert_eq!(sinks.len(), 2);
    assert!(sinks.contains(&p4.id().to_string()));
    assert!(sinks.contains(&p5.id().to_string()));

    // Upstream of p4 includes p1, p2
    let upstream = runtime
        .graph_optimizer()
        .find_upstream(&p4.id().to_string());
    assert_eq!(upstream.len(), 2);
    assert!(upstream.contains(&p1.id().to_string()));
    assert!(upstream.contains(&p2.id().to_string()));
}

#[test]
fn test_cache_clearing() {
    let mut runtime = StreamRuntime::new();

    let _p1 = runtime.add_processor::<DummyProcessor>().unwrap();

    // Optimize and cache
    runtime.graph_optimizer_mut().optimize();
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 1);

    // Second optimize - cache hit
    runtime.graph_optimizer_mut().optimize();
    assert_eq!(runtime.graph_optimizer().stats().cache_hits, 1);

    // Clear cache
    runtime.graph_optimizer_mut().clear_cache();

    // Next optimize - cache miss (cache was cleared)
    runtime.graph_optimizer_mut().optimize();
    assert_eq!(runtime.graph_optimizer().stats().cache_misses, 2);
}
