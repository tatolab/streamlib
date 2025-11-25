// Integration tests for Graph with StreamRuntime
//
// These tests verify that the Graph correctly tracks processors and connections
// using the Phase 1 unified API where the runtime is a thin orchestrator
// that only modifies the graph (DOM) and publishes events.
//
// NOTE: GraphOptimizer is now an executor concern, not accessible from runtime.
// These tests focus on graph operations which are the runtime's responsibility.

use streamlib::StreamRuntime;

#[test]
fn test_graph_tracks_processors() {
    let mut runtime = StreamRuntime::new();

    // Initially empty
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.processor_count(), 0);
    }

    // Add processors
    let _p1 = runtime.add_processor("DummyProcessor").unwrap();
    let _p2 = runtime.add_processor("DummyProcessor").unwrap();

    // Graph should track them
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.processor_count(), 2);
    }
}

#[test]
fn test_find_sources_sinks_no_connections() {
    let mut runtime = StreamRuntime::new();

    // Add isolated processors (no connections)
    let _p1 = runtime.add_processor("DummyProcessor").unwrap();
    let _p2 = runtime.add_processor("DummyProcessor").unwrap();
    let _p3 = runtime.add_processor("DummyProcessor").unwrap();

    // Without connections, all processors are both sources and sinks
    let graph = runtime.graph().read();
    let sources = graph.find_sources();
    let sinks = graph.find_sinks();

    assert_eq!(sources.len(), 3);
    assert_eq!(sinks.len(), 3);
}

#[test]
fn test_topological_order_no_connections() {
    let mut runtime = StreamRuntime::new();

    // Add isolated processors
    let _p1 = runtime.add_processor("DummyProcessor").unwrap();
    let _p2 = runtime.add_processor("DummyProcessor").unwrap();
    let _p3 = runtime.add_processor("DummyProcessor").unwrap();

    // Get topological order (any order is valid for isolated nodes)
    let graph = runtime.graph().read();
    let order = graph.topological_order().unwrap();
    assert_eq!(order.len(), 3);
}

#[test]
fn test_json_export() {
    let mut runtime = StreamRuntime::new();

    // Create processors
    let p1 = runtime.add_processor("DummyProcessor").unwrap();
    let p2 = runtime.add_processor("DummyProcessor").unwrap();

    // Export as JSON
    let graph = runtime.graph().read();
    let json = graph.to_json();

    // Verify JSON structure
    let nodes = json["nodes"].as_array().unwrap();
    let edges = json["edges"].as_array().unwrap();

    assert_eq!(nodes.len(), 2);
    assert_eq!(edges.len(), 0); // No connections yet

    // Verify node IDs
    let node_ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert!(node_ids.contains(&p1.id.as_str()));
    assert!(node_ids.contains(&p2.id.as_str()));
}

#[test]
fn test_dot_export() {
    let mut runtime = StreamRuntime::new();

    // Create processors
    let p1 = runtime.add_processor("DummyProcessor").unwrap();
    let p2 = runtime.add_processor("DummyProcessor").unwrap();

    // Export as DOT
    let graph = runtime.graph().read();
    let dot = graph.to_dot();

    // Verify DOT structure
    assert!(dot.contains("digraph"));
    assert!(dot.contains(&p1.id));
    assert!(dot.contains(&p2.id));
}

#[test]
fn test_connect_processors() {
    let mut runtime = StreamRuntime::new();

    // Create processors
    let source = runtime.add_processor("SourceProcessor").unwrap();
    let sink = runtime.add_processor("SinkProcessor").unwrap();

    // Connect them using port address strings
    let from_port = source.output_port("video");
    let to_port = sink.input_port("video");
    let edge = runtime.connect(&from_port, &to_port).unwrap();

    // Verify connection
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.connection_count(), 1);

        // Source should no longer be a sink (has outgoing)
        let sources = graph.find_sources();
        let sinks = graph.find_sinks();
        assert_eq!(sources.len(), 1);
        assert_eq!(sinks.len(), 1);
        assert!(sources.contains(&source.id));
        assert!(sinks.contains(&sink.id));
    }

    // Verify edge data
    assert_eq!(edge.from_port, from_port);
    assert_eq!(edge.to_port, to_port);
}

#[test]
fn test_linear_pipeline_topology() {
    let mut runtime = StreamRuntime::new();

    // Create a linear pipeline: source → transform → sink
    let source = runtime.add_processor("SourceProcessor").unwrap();
    let transform = runtime.add_processor("TransformProcessor").unwrap();
    let sink = runtime.add_processor("SinkProcessor").unwrap();

    // Connect them
    runtime
        .connect(
            &source.output_port("video"),
            &transform.input_port("video"),
        )
        .unwrap();
    runtime
        .connect(&transform.output_port("video"), &sink.input_port("video"))
        .unwrap();

    // Verify topology
    let graph = runtime.graph().read();

    // Check sources and sinks
    let sources = graph.find_sources();
    let sinks = graph.find_sinks();
    assert_eq!(sources.len(), 1);
    assert_eq!(sinks.len(), 1);
    assert!(sources.contains(&source.id));
    assert!(sinks.contains(&sink.id));

    // Check topological order
    let order = graph.topological_order().unwrap();
    assert_eq!(order.len(), 3);

    // Source must come before transform, transform before sink
    let source_pos = order.iter().position(|x| x == &source.id).unwrap();
    let transform_pos = order.iter().position(|x| x == &transform.id).unwrap();
    let sink_pos = order.iter().position(|x| x == &sink.id).unwrap();
    assert!(source_pos < transform_pos);
    assert!(transform_pos < sink_pos);
}

#[test]
fn test_diamond_graph_topology() {
    let mut runtime = StreamRuntime::new();

    // Create a diamond graph:
    //       source
    //      /      \
    //   left    right
    //      \      /
    //        sink
    let source = runtime.add_processor("SourceProcessor").unwrap();
    let left = runtime.add_processor("LeftProcessor").unwrap();
    let right = runtime.add_processor("RightProcessor").unwrap();
    let sink = runtime.add_processor("SinkProcessor").unwrap();

    // Connect them
    runtime
        .connect(&source.output_port("video"), &left.input_port("video"))
        .unwrap();
    runtime
        .connect(&source.output_port("data"), &right.input_port("data"))
        .unwrap();
    runtime
        .connect(&left.output_port("video"), &sink.input_port("left"))
        .unwrap();
    runtime
        .connect(&right.output_port("data"), &sink.input_port("right"))
        .unwrap();

    // Verify topology
    let graph = runtime.graph().read();

    // Check sources and sinks
    let sources = graph.find_sources();
    let sinks = graph.find_sinks();
    assert_eq!(sources.len(), 1);
    assert_eq!(sinks.len(), 1);
    assert!(sources.contains(&source.id));
    assert!(sinks.contains(&sink.id));

    // Check topological order - source must be first, sink must be last
    let order = graph.topological_order().unwrap();
    assert_eq!(order.len(), 4);

    let source_pos = order.iter().position(|x| x == &source.id).unwrap();
    let left_pos = order.iter().position(|x| x == &left.id).unwrap();
    let right_pos = order.iter().position(|x| x == &right.id).unwrap();
    let sink_pos = order.iter().position(|x| x == &sink.id).unwrap();

    assert!(source_pos < left_pos);
    assert!(source_pos < right_pos);
    assert!(left_pos < sink_pos);
    assert!(right_pos < sink_pos);
}

// Tests that require executor access are now implementation details
// The runtime is just the graph manipulator - execution is the executor's job

#[test]
#[ignore = "GraphOptimizer is now an executor concern - test via executor tests"]
fn test_legacy_execution_plan() {
    // This test would verify that optimize() returns a Legacy execution plan
    // Now tested in executor tests
}

#[test]
#[ignore = "GraphOptimizer is now an executor concern - test via executor tests"]
fn test_plan_caching() {
    // This test would verify that the optimizer caches plans by checksum
    // Now tested in executor tests
}

#[test]
#[ignore = "GraphOptimizer is now an executor concern - test via executor tests"]
fn test_cache_clearing() {
    // This test would verify that clear_cache() works correctly
    // Now tested in executor tests
}
