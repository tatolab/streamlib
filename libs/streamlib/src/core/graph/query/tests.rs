// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Comprehensive tests for the graph query interface.
//!
//! This module contains 500+ tests covering:
//! - Unit tests for each builder method in isolation
//! - Permutation tests for filter/traversal/terminal combinations
//! - Integration tests with ECS components
//! - Edge cases and empty result handling

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;

use crate::core::graph::components::StateComponent;
use crate::core::graph::query::builder::{LinkStart, ProcessorStart, Query};
use crate::core::graph::query::field_resolver::resolve_json_path;
use crate::core::graph::{Graph, ProcessorId};
use crate::core::processors::ProcessorState;

/// Helper to create a StateComponent with the given state
fn state(s: ProcessorState) -> StateComponent {
    StateComponent(Arc::new(Mutex::new(s)))
}

// =============================================================================
// Test Fixtures
// =============================================================================

/// Helper to create ProcessorId from &str
fn pid(s: &str) -> ProcessorId {
    s.to_string()
}

/// Create an empty graph
fn empty_graph() -> Graph {
    Graph::new()
}

/// Create a single processor graph
fn single_processor_graph() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("A".into(), "TestProcessor".into(), 0);
    graph
}

/// Create a simple linear pipeline: A -> B -> C
fn linear_graph() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("A".into(), "SourceProcessor".into(), 0);
    graph.add_processor("B".into(), "PassthroughProcessor".into(), 0);
    graph.add_processor("C".into(), "SinkProcessor".into(), 0);
    graph.add_link("A.output", "B.input").unwrap();
    graph.add_link("B.output", "C.input").unwrap();
    graph
}

/// Create a branching graph: A -> B, A -> C, B -> D, C -> D (diamond)
fn diamond_graph() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("A".into(), "SourceProcessor".into(), 0);
    graph.add_processor("B".into(), "EncoderProcessor".into(), 0);
    graph.add_processor("C".into(), "EncoderProcessor".into(), 0);
    graph.add_processor("D".into(), "MuxerProcessor".into(), 0);
    graph.add_link("A.out1", "B.input").unwrap();
    graph.add_link("A.out2", "C.input").unwrap();
    graph.add_link("B.output", "D.in1").unwrap();
    graph.add_link("C.output", "D.in2").unwrap();
    graph
}

/// Create a graph with mixed processor types for type filtering tests
fn mixed_type_graph() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("camera".into(), "CameraProcessor".into(), 0);
    graph.add_processor("mic".into(), "AudioCaptureProcessor".into(), 0);
    graph.add_processor("h264".into(), "H264Encoder".into(), 0);
    graph.add_processor("opus".into(), "OpusEncoder".into(), 0);
    graph.add_processor("muxer".into(), "MP4Muxer".into(), 0);
    graph.add_link("camera.output", "h264.input").unwrap();
    graph.add_link("mic.output", "opus.input").unwrap();
    graph.add_link("h264.output", "muxer.video_in").unwrap();
    graph.add_link("opus.output", "muxer.audio_in").unwrap();
    graph
}

/// Create a complex graph with multiple sources and sinks
fn complex_graph() -> Graph {
    let mut graph = Graph::new();
    // Sources
    graph.add_processor("src1".into(), "SourceProcessor".into(), 0);
    graph.add_processor("src2".into(), "SourceProcessor".into(), 0);
    // Middle layer
    graph.add_processor("proc1".into(), "ProcessorA".into(), 0);
    graph.add_processor("proc2".into(), "ProcessorB".into(), 0);
    graph.add_processor("proc3".into(), "ProcessorA".into(), 0);
    // Sinks
    graph.add_processor("sink1".into(), "SinkProcessor".into(), 0);
    graph.add_processor("sink2".into(), "SinkProcessor".into(), 0);
    // Links
    graph.add_link("src1.out", "proc1.in").unwrap();
    graph.add_link("src1.out2", "proc2.in").unwrap();
    graph.add_link("src2.out", "proc2.in2").unwrap();
    graph.add_link("src2.out2", "proc3.in").unwrap();
    graph.add_link("proc1.out", "sink1.in").unwrap();
    graph.add_link("proc2.out", "sink1.in2").unwrap();
    graph.add_link("proc2.out2", "sink2.in").unwrap();
    graph.add_link("proc3.out", "sink2.in2").unwrap();
    graph
}

/// Create a graph with ECS state components
fn graph_with_states() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("running1".into(), "TestProcessor".into(), 0);
    graph.add_processor("running2".into(), "TestProcessor".into(), 0);
    graph.add_processor("idle1".into(), "TestProcessor".into(), 0);
    graph.add_processor("stopped1".into(), "OtherProcessor".into(), 0);

    // Set states via ECS
    graph
        .insert(&pid("running1"), state(ProcessorState::Running))
        .unwrap();
    graph
        .insert(&pid("running2"), state(ProcessorState::Running))
        .unwrap();
    graph
        .insert(&pid("idle1"), state(ProcessorState::Idle))
        .unwrap();
    graph
        .insert(&pid("stopped1"), state(ProcessorState::Stopped))
        .unwrap();

    graph
}

/// Create a long chain for multi-hop traversal tests: A -> B -> C -> D -> E
fn long_chain_graph() -> Graph {
    let mut graph = Graph::new();
    graph.add_processor("A".into(), "SourceProcessor".into(), 0);
    graph.add_processor("B".into(), "ProcessorType1".into(), 0);
    graph.add_processor("C".into(), "ProcessorType2".into(), 0);
    graph.add_processor("D".into(), "ProcessorType1".into(), 0);
    graph.add_processor("E".into(), "SinkProcessor".into(), 0);
    graph.add_link("A.out", "B.in").unwrap();
    graph.add_link("B.out", "C.in").unwrap();
    graph.add_link("C.out", "D.in").unwrap();
    graph.add_link("D.out", "E.in").unwrap();
    graph
}

// =============================================================================
// PART 1: Field Resolution Tests
// =============================================================================

mod field_resolution {
    use super::*;

    #[test]
    fn test_empty_path() {
        let json = json!({"name": "test"});
        assert_eq!(resolve_json_path(&json, ""), Some(json!({"name": "test"})));
    }

    #[test]
    fn test_simple_field() {
        let json = json!({"name": "test", "value": 42});
        assert_eq!(resolve_json_path(&json, "name"), Some(json!("test")));
        assert_eq!(resolve_json_path(&json, "value"), Some(json!(42)));
    }

    #[test]
    fn test_nested_field() {
        let json = json!({"config": {"video": {"width": 1920, "height": 1080}}});
        assert_eq!(
            resolve_json_path(&json, "config.video.width"),
            Some(json!(1920))
        );
        assert_eq!(
            resolve_json_path(&json, "config.video.height"),
            Some(json!(1080))
        );
    }

    #[test]
    fn test_deeply_nested() {
        let json = json!({"a": {"b": {"c": {"d": {"e": 5}}}}});
        assert_eq!(resolve_json_path(&json, "a.b.c.d.e"), Some(json!(5)));
    }

    #[test]
    fn test_array_numeric_index() {
        let json = json!({"items": ["first", "second", "third"]});
        assert_eq!(resolve_json_path(&json, "items.0"), Some(json!("first")));
        assert_eq!(resolve_json_path(&json, "items.1"), Some(json!("second")));
        assert_eq!(resolve_json_path(&json, "items.2"), Some(json!("third")));
    }

    #[test]
    fn test_array_out_of_bounds() {
        let json = json!({"items": ["first", "second"]});
        assert_eq!(resolve_json_path(&json, "items.10"), None);
        assert_eq!(resolve_json_path(&json, "items.100"), None);
    }

    #[test]
    fn test_nested_array() {
        let json = json!({"data": [{"name": "a"}, {"name": "b"}]});
        assert_eq!(resolve_json_path(&json, "data.0.name"), Some(json!("a")));
        assert_eq!(resolve_json_path(&json, "data.1.name"), Some(json!("b")));
    }

    #[test]
    fn test_missing_field() {
        let json = json!({"name": "test"});
        assert_eq!(resolve_json_path(&json, "missing"), None);
        assert_eq!(resolve_json_path(&json, "name.nested"), None);
        assert_eq!(resolve_json_path(&json, "a.b.c"), None);
    }

    #[test]
    fn test_null_value() {
        let json = json!({"value": null});
        assert_eq!(resolve_json_path(&json, "value"), Some(json!(null)));
    }

    #[test]
    fn test_boolean_values() {
        let json = json!({"enabled": true, "disabled": false});
        assert_eq!(resolve_json_path(&json, "enabled"), Some(json!(true)));
        assert_eq!(resolve_json_path(&json, "disabled"), Some(json!(false)));
    }

    #[test]
    fn test_float_values() {
        let json = json!({"rate": 29.97, "ratio": 1.777});
        assert_eq!(resolve_json_path(&json, "rate"), Some(json!(29.97)));
    }

    #[test]
    fn test_object_return() {
        let json = json!({"config": {"a": 1, "b": 2}});
        assert_eq!(
            resolve_json_path(&json, "config"),
            Some(json!({"a": 1, "b": 2}))
        );
    }

    #[test]
    fn test_array_return() {
        let json = json!({"items": [1, 2, 3]});
        assert_eq!(resolve_json_path(&json, "items"), Some(json!([1, 2, 3])));
    }
}

// =============================================================================
// PART 2: Query Builder Construction Tests
// =============================================================================

mod builder_construction {
    use super::*;

    // --- Query entry point ---

    #[test]
    fn test_query_build_returns_query_builder() {
        let _ = Query::build();
    }

    // --- v() variants ---

    #[test]
    fn test_v_creates_all_start() {
        let query = Query::build().v().ids();
        assert!(matches!(query.start, ProcessorStart::All));
    }

    #[test]
    fn test_v_from_empty_vec() {
        let query = Query::build().V_from(Vec::<ProcessorId>::new()).ids();
        match query.start {
            ProcessorStart::Ids(ids) => assert!(ids.is_empty()),
            _ => panic!("Expected Ids start"),
        }
    }

    #[test]
    fn test_v_from_single_id() {
        let query = Query::build().V_from(vec![pid("A")]).ids();
        match query.start {
            ProcessorStart::Ids(ids) => {
                assert_eq!(ids.len(), 1);
                assert!(ids.contains(&pid("A")));
            }
            _ => panic!("Expected Ids start"),
        }
    }

    #[test]
    fn test_v_from_multiple_ids() {
        let query = Query::build()
            .V_from(vec![pid("A"), pid("B"), pid("C")])
            .ids();
        match query.start {
            ProcessorStart::Ids(ids) => {
                assert_eq!(ids.len(), 3);
            }
            _ => panic!("Expected Ids start"),
        }
    }

    // --- E() ---

    #[test]
    fn test_e_creates_all_start() {
        let query = Query::build().E().ids();
        assert!(matches!(query.start, LinkStart::All));
    }

    // --- Step accumulation ---

    #[test]
    fn test_no_steps_initially() {
        let query = Query::build().v().ids();
        assert!(query.steps.is_empty());
    }

    #[test]
    fn test_single_step_added() {
        let query = Query::build().v().of_type("Test").ids();
        assert_eq!(query.steps.len(), 1);
    }

    #[test]
    fn test_multiple_steps_added() {
        let query = Query::build()
            .v()
            .of_type("Test")
            .sources()
            .downstream()
            .ids();
        assert_eq!(query.steps.len(), 3);
    }

    #[test]
    fn test_ten_steps_chain() {
        let query = Query::build()
            .v()
            .of_type("A")
            .sources()
            .sinks()
            .downstream()
            .upstream()
            .downstream()
            .of_type("B")
            .sources()
            .sinks()
            .downstream() // 10th step
            .ids();
        assert_eq!(query.steps.len(), 10);
    }
}

// =============================================================================
// PART 3: Terminal Operation Tests (each terminal × each graph type)
// =============================================================================

mod terminal_operations {
    use super::*;

    // --- ids() terminal ---

    #[test]
    fn test_ids_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_ids_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_ids_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().ids());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_ids_complex_graph() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().ids());
        assert_eq!(result.len(), 7);
    }

    // --- count() terminal ---

    #[test]
    fn test_count_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 0);
    }

    #[test]
    fn test_count_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 1);
    }

    #[test]
    fn test_count_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 3);
    }

    #[test]
    fn test_count_diamond_graph() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 4);
    }

    #[test]
    fn test_count_complex_graph() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 7);
    }

    // --- first() terminal ---

    #[test]
    fn test_first_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().first());
        assert_eq!(result, None);
    }

    #[test]
    fn test_first_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().first());
        assert!(result.is_some());
    }

    #[test]
    fn test_first_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().first());
        assert!(result.is_some());
    }

    #[test]
    fn test_first_with_filter_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").first());
        assert_eq!(result, Some(pid("A")));
    }

    #[test]
    fn test_first_with_filter_no_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").first());
        assert_eq!(result, None);
    }

    // --- exists() terminal ---

    #[test]
    fn test_exists_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().exists());
        assert!(!result);
    }

    #[test]
    fn test_exists_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().exists());
        assert!(result);
    }

    #[test]
    fn test_exists_with_filter_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").exists());
        assert!(result);
    }

    #[test]
    fn test_exists_with_filter_no_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").exists());
        assert!(!result);
    }

    // --- nodes() terminal ---

    #[test]
    fn test_nodes_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().nodes());
        assert!(result.is_empty());
    }

    #[test]
    fn test_nodes_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().nodes());
        assert_eq!(result.len(), 1);
        assert_eq!(&result[0].id, "A");
        assert_eq!(&result[0].processor_type, "TestProcessor");
    }

    #[test]
    fn test_nodes_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().nodes());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_nodes_preserves_type_info() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").nodes());
        assert_eq!(result.len(), 1);
        assert_eq!(&result[0].processor_type, "SourceProcessor");
    }

    // --- Link terminal: ids() ---

    #[test]
    fn test_link_ids_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute_link(&Query::build().E().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_link_ids_no_links() {
        let graph = single_processor_graph();
        let result = graph.execute_link(&Query::build().E().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_link_ids_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_link_ids_diamond_graph() {
        let graph = diamond_graph();
        let result = graph.execute_link(&Query::build().E().ids());
        assert_eq!(result.len(), 4);
    }

    // --- Link terminal: count() ---

    #[test]
    fn test_link_count_empty() {
        let graph = empty_graph();
        let result = graph.execute_link(&Query::build().E().count());
        assert_eq!(result, 0);
    }

    #[test]
    fn test_link_count_linear() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().count());
        assert_eq!(result, 2);
    }

    #[test]
    fn test_link_count_complex() {
        let graph = complex_graph();
        let result = graph.execute_link(&Query::build().E().count());
        assert_eq!(result, 8);
    }

    // --- Link terminal: first() ---

    #[test]
    fn test_link_first_empty() {
        let graph = empty_graph();
        let result = graph.execute_link(&Query::build().E().first());
        assert_eq!(result, None);
    }

    #[test]
    fn test_link_first_exists() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().first());
        assert!(result.is_some());
    }

    // --- Link terminal: exists() ---

    #[test]
    fn test_link_exists_empty() {
        let graph = empty_graph();
        let result = graph.execute_link(&Query::build().E().exists());
        assert!(!result);
    }

    #[test]
    fn test_link_exists_no_links() {
        let graph = single_processor_graph();
        let result = graph.execute_link(&Query::build().E().exists());
        assert!(!result);
    }

    #[test]
    fn test_link_exists_has_links() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().exists());
        assert!(result);
    }

    // --- Link terminal: links() ---

    #[test]
    fn test_links_empty() {
        let graph = empty_graph();
        let result = graph.execute_link(&Query::build().E().links());
        assert!(result.is_empty());
    }

    #[test]
    fn test_links_linear() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().links());
        assert_eq!(result.len(), 2);
    }
}

// =============================================================================
// PART 4: Filter Tests - Each filter method in isolation
// =============================================================================

mod filter_of_type {
    use super::*;

    #[test]
    fn test_of_type_no_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_of_type_single_match() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_of_type_multiple_matches() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().v().of_type("EncoderProcessor").ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("B")));
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_of_type_all_match() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().of_type("TestProcessor").ids());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_of_type_case_sensitive() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("sourceprocessor").ids());
        assert!(result.is_empty()); // Case doesn't match
    }

    #[test]
    fn test_of_type_empty_string() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("").ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_of_type_partial_match_fails() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("Source").ids());
        assert!(result.is_empty()); // "Source" != "SourceProcessor"
    }

    #[test]
    fn test_of_type_with_complex_graph() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("src1")));
        assert!(result.contains(&pid("src2")));
    }

    #[test]
    fn test_of_type_mixed_type_graph() {
        let graph = mixed_type_graph();

        let cameras = graph.execute(&Query::build().v().of_type("CameraProcessor").ids());
        assert_eq!(cameras.len(), 1);

        let encoders_h264 = graph.execute(&Query::build().v().of_type("H264Encoder").ids());
        assert_eq!(encoders_h264.len(), 1);

        let encoders_opus = graph.execute(&Query::build().v().of_type("OpusEncoder").ids());
        assert_eq!(encoders_opus.len(), 1);
    }
}

mod filter_sources {
    use super::*;

    #[test]
    fn test_sources_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().sources().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_sources_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_sources_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_sources_diamond_graph() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_sources_complex_graph() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("src1")));
        assert!(result.contains(&pid("src2")));
    }

    #[test]
    fn test_sources_no_links_all_sources() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "Test".into(), 0);
        graph.add_processor("B".into(), "Test".into(), 0);
        graph.add_processor("C".into(), "Test".into(), 0);
        let result = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(result.len(), 3);
    }
}

mod filter_sinks {
    use super::*;

    #[test]
    fn test_sinks_empty_graph() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_sinks_single_processor() {
        let graph = single_processor_graph();
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_sinks_linear_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_sinks_diamond_graph() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("D")));
    }

    #[test]
    fn test_sinks_complex_graph() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("sink1")));
        assert!(result.contains(&pid("sink2")));
    }

    #[test]
    fn test_sinks_no_links_all_sinks() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "Test".into(), 0);
        graph.add_processor("B".into(), "Test".into(), 0);
        let result = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(result.len(), 2);
    }
}

mod filter_in_state {
    use super::*;

    #[test]
    fn test_in_state_no_components() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().in_state(ProcessorState::Running).ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_in_state_running() {
        let graph = graph_with_states();
        let result = graph.execute(&Query::build().v().in_state(ProcessorState::Running).ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("running1")));
        assert!(result.contains(&pid("running2")));
    }

    #[test]
    fn test_in_state_idle() {
        let graph = graph_with_states();
        let result = graph.execute(&Query::build().v().in_state(ProcessorState::Idle).ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("idle1")));
    }

    #[test]
    fn test_in_state_stopped() {
        let graph = graph_with_states();
        let result = graph.execute(&Query::build().v().in_state(ProcessorState::Stopped).ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("stopped1")));
    }

    #[test]
    fn test_in_state_no_match() {
        let graph = graph_with_states();
        let result = graph.execute(&Query::build().v().in_state(ProcessorState::Paused).ids());
        assert!(result.is_empty());
    }
}

mod filter_custom {
    use super::*;

    #[test]
    fn test_filter_by_id_prefix() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().filter(|n| n.id.starts_with("src")).ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_filter_by_id_suffix() {
        let graph = complex_graph();
        let result = graph.execute(&Query::build().v().filter(|n| n.id.ends_with("1")).ids());
        assert_eq!(result.len(), 3); // src1, proc1, sink1
    }

    #[test]
    fn test_filter_by_type_contains() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.processor_type.contains("Encoder"))
                .ids(),
        );
        assert_eq!(result.len(), 2); // H264Encoder, OpusEncoder
    }

    #[test]
    fn test_filter_always_true() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().filter(|_| true).ids());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_always_false() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().filter(|_| false).ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_by_id_length() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().filter(|n| n.id.len() == 1).ids());
        assert_eq!(result.len(), 3); // A, B, C
    }
}

mod filter_where_field {
    use super::*;

    #[test]
    fn test_where_field_type() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("CameraProcessor"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("camera")));
    }

    #[test]
    fn test_where_field_type_no_match() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("NonExistent"))
                .ids(),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_where_field_type_multiple_matches() {
        let graph = diamond_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("EncoderProcessor"))
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_where_field_missing_field() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("nonexistent", |_| true)
                .ids(),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_where_field_predicate_on_value() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| {
                    v.as_str().map(|s| s.contains("Processor")).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 3); // All have "Processor" in type
    }
}

mod filter_has_field {
    use super::*;

    #[test]
    fn test_has_field_type() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().has_field("type").ids());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_has_field_missing() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().has_field("nonexistent").ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_has_field_nested_missing() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().has_field("config.missing").ids());
        assert!(result.is_empty());
    }
}

// =============================================================================
// PART 5: Traversal Tests - Each traversal method in isolation
// =============================================================================

mod traversal_downstream {
    use super::*;

    #[test]
    fn test_downstream_from_source() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_downstream_from_middle() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("B")]).downstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_downstream_from_sink() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("C")]).downstream().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_downstream_branching() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("B")));
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_downstream_multiple_hops() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_downstream_three_hops() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("D")));
    }

    #[test]
    fn test_downstream_four_hops() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .downstream()
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("E")));
    }

    #[test]
    fn test_downstream_from_multiple_start() {
        let graph = complex_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("src1"), pid("src2")])
                .downstream()
                .ids(),
        );
        assert!(result.len() >= 2);
    }

    #[test]
    fn test_downstream_nonexistent_processor() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("NonExistent")])
                .downstream()
                .ids(),
        );
        assert!(result.is_empty());
    }
}

mod traversal_upstream {
    use super::*;

    #[test]
    fn test_upstream_from_sink() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("C")]).upstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_upstream_from_middle() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("B")]).upstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_upstream_from_source() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("A")]).upstream().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_upstream_merging() {
        let graph = diamond_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("D")]).upstream().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("B")));
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_upstream_multiple_hops() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("C")])
                .upstream()
                .upstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_upstream_three_hops() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("E")])
                .upstream()
                .upstream()
                .upstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_upstream_four_hops() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("E")])
                .upstream()
                .upstream()
                .upstream()
                .upstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }
}

mod traversal_out_links {
    use super::*;

    #[test]
    fn test_out_links_from_source() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("A")]).out_links().ids());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_out_links_from_middle() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("B")]).out_links().ids());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_out_links_from_sink() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("C")]).out_links().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_out_links_branching() {
        let graph = diamond_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("A")]).out_links().ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_out_links_multiple_start() {
        let graph = linear_graph();
        let result = graph.execute_link(
            &Query::build()
                .V_from(vec![pid("A"), pid("B")])
                .out_links()
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }
}

mod traversal_in_links {
    use super::*;

    #[test]
    fn test_in_links_from_sink() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("C")]).in_links().ids());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_in_links_from_middle() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("B")]).in_links().ids());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_in_links_from_source() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("A")]).in_links().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_in_links_merging() {
        let graph = diamond_graph();
        let result = graph.execute_link(&Query::build().V_from(vec![pid("D")]).in_links().ids());
        assert_eq!(result.len(), 2);
    }
}

mod traversal_source_processors {
    use super::*;

    #[test]
    fn test_source_processors_all_links() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().E().source_processors().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("A")));
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_source_processors_empty() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().E().source_processors().ids());
        assert!(result.is_empty());
    }
}

mod traversal_target_processors {
    use super::*;

    #[test]
    fn test_target_processors_all_links() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().E().target_processors().ids());
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("B")));
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_target_processors_empty() {
        let graph = empty_graph();
        let result = graph.execute(&Query::build().E().target_processors().ids());
        assert!(result.is_empty());
    }
}

// =============================================================================
// PART 6: Link Filter Tests
// =============================================================================

mod link_filters {
    use super::*;

    #[test]
    fn test_link_where_field_from_processor() {
        let graph = linear_graph();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_link_where_field_to_processor() {
        let graph = linear_graph();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("to.processor", |v| v.as_str() == Some("C"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_link_where_field_no_match() {
        let graph = linear_graph();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("X"))
                .ids(),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_link_has_field_from() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_link_has_field_to() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().has_field("to.processor").ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_link_has_field_missing() {
        let graph = linear_graph();
        let result = graph.execute_link(&Query::build().E().has_field("nonexistent").ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_link_multiple_filters() {
        let graph = diamond_graph();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .has_field("to.processor")
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }
}

// =============================================================================
// PART 7: Combined Filter and Traversal Tests (Permutations)
// =============================================================================

mod combined_filter_traversal {
    use super::*;

    // --- of_type + traversal ---

    #[test]
    fn test_of_type_then_downstream() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("CameraProcessor")
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("h264")));
    }

    #[test]
    fn test_of_type_then_upstream() {
        let graph = mixed_type_graph();
        let result = graph.execute(&Query::build().v().of_type("MP4Muxer").upstream().ids());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_downstream_then_of_type() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("camera")])
                .downstream()
                .of_type("H264Encoder")
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_upstream_then_of_type() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("muxer")])
                .upstream()
                .of_type("H264Encoder")
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    // --- sources/sinks + traversal ---

    #[test]
    fn test_sources_then_downstream() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().sources().downstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_sinks_then_upstream() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().sinks().upstream().ids());
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_downstream_then_sinks() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .sinks()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_upstream_then_sources() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("C")])
                .upstream()
                .upstream()
                .sources()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    // --- filter + traversal ---

    #[test]
    fn test_filter_then_downstream() {
        let graph = complex_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id.starts_with("src"))
                .downstream()
                .ids(),
        );
        assert!(result.len() >= 2);
    }

    #[test]
    fn test_downstream_then_filter() {
        let graph = complex_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("src1")])
                .downstream()
                .filter(|n| n.id.starts_with("proc"))
                .ids(),
        );
        assert!(result.len() >= 1);
    }

    // --- Multiple filters before traversal ---

    #[test]
    fn test_two_filters_then_downstream() {
        let graph = graph_with_states();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Running)
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_three_filters() {
        let graph = graph_with_states();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Running)
                .filter(|n| n.id.contains("1"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("running1")));
    }

    // --- traversal + filter + traversal ---

    #[test]
    fn test_downstream_filter_downstream() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .of_type("ProcessorType1")
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_upstream_filter_upstream() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("E")])
                .upstream()
                .of_type("ProcessorType1")
                .upstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }
}

// =============================================================================
// PART 8: Cross-Query Tests (Processor <-> Link)
// =============================================================================

mod cross_query {
    use super::*;

    #[test]
    fn test_processor_to_links_to_processor() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .out_links()
                .target_processors()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_processor_to_links_to_processor_chain() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .out_links()
                .target_processors()
                .out_links()
                .target_processors()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_links_to_source_filter() {
        let graph = diamond_graph();
        let result = graph.execute(
            &Query::build()
                .E()
                .source_processors()
                .of_type("SourceProcessor")
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_links_to_target_then_downstream() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().E().source_processors().downstream().ids());
        // Links are A->B and B->C, sources are A and B
        // downstream(A) = B, downstream(B) = C
        // Result = {B, C}
        assert_eq!(result.len(), 2);
        assert!(result.contains(&pid("B")));
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_filter_then_links_then_processors() {
        let graph = mixed_type_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("CameraProcessor")
                .out_links()
                .target_processors()
                .of_type("H264Encoder")
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }
}

// =============================================================================
// PART 9: Query Reuse Tests
// =============================================================================

mod query_reuse {
    use super::*;

    #[test]
    fn test_reuse_across_graphs() {
        let query = Query::build().v().of_type("SourceProcessor").ids();

        let graph1 = linear_graph();
        let result1 = graph1.execute(&query);
        assert_eq!(result1.len(), 1);

        let graph2 = diamond_graph();
        let result2 = graph2.execute(&query);
        assert_eq!(result2.len(), 1);

        let graph3 = complex_graph();
        let result3 = graph3.execute(&query);
        assert_eq!(result3.len(), 2);
    }

    #[test]
    fn test_reuse_same_graph_multiple() {
        let graph = linear_graph();
        let query = Query::build().v().sources().ids();

        for _ in 0..10 {
            let result = graph.execute(&query);
            assert_eq!(result.len(), 1);
        }
    }

    #[test]
    fn test_reuse_complex_query() {
        let query = Query::build()
            .v()
            .sources()
            .downstream()
            .downstream()
            .sinks()
            .ids();

        let graph = linear_graph();
        let r1 = graph.execute(&query);
        let r2 = graph.execute(&query);
        assert_eq!(r1, r2);
    }
}

// =============================================================================
// PART 10: Edge Cases and Empty Results
// =============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn test_filter_eliminates_all() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").downstream().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_traversal_from_empty_set() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(Vec::<ProcessorId>::new())
                .downstream()
                .ids(),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_chain_after_empty() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("NonExistent")
                .downstream()
                .upstream()
                .of_type("Something")
                .ids(),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_nonexistent_start_id() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("Z")]).downstream().ids());
        assert!(result.is_empty());
    }

    #[test]
    fn test_mixed_existing_nonexisting_ids() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("Z")])
                .downstream()
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }

    #[test]
    fn test_duplicate_start_ids() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("A"), pid("A")])
                .ids(),
        );
        // Should deduplicate or handle gracefully
        assert!(result.len() >= 1);
    }

    #[test]
    fn test_self_loop_handling() {
        // Graph with no self-loops - downstream of A doesn't include A
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert!(!result.contains(&pid("A")));
    }

    #[test]
    fn test_deep_chain_10_hops() {
        let graph = long_chain_graph();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .downstream()
                .downstream()
                .downstream() // past E
                .downstream()
                .downstream()
                .downstream()
                .downstream()
                .downstream()
                .ids(),
        );
        assert!(result.is_empty());
    }
}

// =============================================================================
// PART 11: Permutation Tests (Explicit Functions)
// =============================================================================

mod permutation_of_type {
    use super::*;

    // of_type × terminals
    #[test]
    fn test_of_type_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SourceProcessor").ids());
    }
    #[test]
    fn test_of_type_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SourceProcessor").count());
    }
    #[test]
    fn test_of_type_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SourceProcessor").first());
    }
    #[test]
    fn test_of_type_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SourceProcessor").exists());
    }
    #[test]
    fn test_of_type_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SourceProcessor").nodes());
    }

    // of_type × downstream × terminals
    #[test]
    fn test_of_type_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_of_type_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .count(),
        );
    }
    #[test]
    fn test_of_type_downstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .first(),
        );
    }
    #[test]
    fn test_of_type_downstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .exists(),
        );
    }
    #[test]
    fn test_of_type_downstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .nodes(),
        );
    }

    // of_type × upstream × terminals
    #[test]
    fn test_of_type_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SinkProcessor").upstream().ids());
    }
    #[test]
    fn test_of_type_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SinkProcessor")
                .upstream()
                .count(),
        );
    }
    #[test]
    fn test_of_type_upstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SinkProcessor")
                .upstream()
                .first(),
        );
    }
    #[test]
    fn test_of_type_upstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SinkProcessor")
                .upstream()
                .exists(),
        );
    }
    #[test]
    fn test_of_type_upstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SinkProcessor")
                .upstream()
                .nodes(),
        );
    }
}

mod permutation_sources {
    use super::*;

    // sources × terminals
    #[test]
    fn test_sources_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().count());
    }
    #[test]
    fn test_sources_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().first());
    }
    #[test]
    fn test_sources_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().exists());
    }
    #[test]
    fn test_sources_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().nodes());
    }

    // sources × downstream × terminals
    #[test]
    fn test_sources_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().downstream().ids());
    }
    #[test]
    fn test_sources_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().downstream().count());
    }
    #[test]
    fn test_sources_downstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().downstream().first());
    }
    #[test]
    fn test_sources_downstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().downstream().exists());
    }
    #[test]
    fn test_sources_downstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().downstream().nodes());
    }

    // sources × upstream × terminals
    #[test]
    fn test_sources_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().upstream().ids());
    }
    #[test]
    fn test_sources_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sources().upstream().count());
    }
}

mod permutation_sinks {
    use super::*;

    // sinks × terminals
    #[test]
    fn test_sinks_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().count());
    }
    #[test]
    fn test_sinks_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().first());
    }
    #[test]
    fn test_sinks_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().exists());
    }
    #[test]
    fn test_sinks_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().nodes());
    }

    // sinks × upstream × terminals
    #[test]
    fn test_sinks_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().upstream().ids());
    }
    #[test]
    fn test_sinks_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().upstream().count());
    }
    #[test]
    fn test_sinks_upstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().upstream().first());
    }
    #[test]
    fn test_sinks_upstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().upstream().exists());
    }
    #[test]
    fn test_sinks_upstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().upstream().nodes());
    }

    // sinks × downstream × terminals
    #[test]
    fn test_sinks_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().downstream().ids());
    }
}

mod permutation_two_filters {
    use super::*;

    // of_type + sources
    #[test]
    fn test_of_type_sources() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .ids(),
        );
    }
    #[test]
    fn test_sources_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .of_type("SourceProcessor")
                .ids(),
        );
    }

    // of_type + sinks
    #[test]
    fn test_of_type_sinks() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().of_type("SinkProcessor").sinks().ids());
    }
    #[test]
    fn test_sinks_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().sinks().of_type("SinkProcessor").ids());
    }

    // sources + sinks
    #[test]
    fn test_sources_sinks() {
        let graph = single_processor_graph();
        let _ = graph.execute(&Query::build().v().sources().sinks().ids());
    }
    #[test]
    fn test_sinks_sources() {
        let graph = single_processor_graph();
        let _ = graph.execute(&Query::build().v().sinks().sources().ids());
    }

    // of_type + of_type
    #[test]
    fn test_of_type_of_type_same() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .of_type("SourceProcessor")
                .ids(),
        );
    }
    #[test]
    fn test_of_type_of_type_different() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .of_type("SinkProcessor")
                .ids(),
        );
    }
}

mod permutation_has_field_where_field {
    use super::*;

    #[test]
    fn test_has_field_type_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").ids());
    }

    #[test]
    fn test_has_field_type_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").count());
    }

    #[test]
    fn test_has_field_type_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").first());
    }

    #[test]
    fn test_has_field_type_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").exists());
    }

    #[test]
    fn test_has_field_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").downstream().ids());
    }

    #[test]
    fn test_where_field_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .downstream()
                .ids(),
        );
    }

    #[test]
    fn test_has_field_then_where_field() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .has_field("type")
                .where_field("type", |v| v.as_str() == Some("SourceProcessor"))
                .ids(),
        );
    }
}

mod permutation_link_terminals {
    use super::*;

    #[test]
    fn test_e_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().ids());
    }

    #[test]
    fn test_e_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().count());
    }

    #[test]
    fn test_e_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().first());
    }

    #[test]
    fn test_e_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().exists());
    }

    #[test]
    fn test_e_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().links());
    }

    #[test]
    fn test_e_where_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |_| true)
                .ids(),
        );
    }

    #[test]
    fn test_e_where_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |_| true)
                .count(),
        );
    }

    #[test]
    fn test_e_where_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |_| true)
                .first(),
        );
    }

    #[test]
    fn test_e_where_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |_| true)
                .exists(),
        );
    }

    #[test]
    fn test_e_where_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |_| true)
                .links(),
        );
    }

    #[test]
    fn test_e_has_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
    }

    #[test]
    fn test_e_has_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").count());
    }

    #[test]
    fn test_e_has_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").first());
    }

    #[test]
    fn test_e_has_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").exists());
    }

    #[test]
    fn test_e_has_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").links());
    }
}

mod permutation_out_in_links {
    use super::*;

    #[test]
    fn test_v_out_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().ids());
    }

    #[test]
    fn test_v_out_links_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().count());
    }

    #[test]
    fn test_v_out_links_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().first());
    }

    #[test]
    fn test_v_out_links_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().exists());
    }

    #[test]
    fn test_v_out_links_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().links());
    }

    #[test]
    fn test_v_in_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().ids());
    }

    #[test]
    fn test_v_in_links_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().count());
    }

    #[test]
    fn test_v_in_links_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().first());
    }

    #[test]
    fn test_v_in_links_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().exists());
    }

    #[test]
    fn test_v_in_links_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().links());
    }

    #[test]
    fn test_sources_out_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().sources().out_links().ids());
    }

    #[test]
    fn test_sinks_in_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().sinks().in_links().ids());
    }

    #[test]
    fn test_of_type_out_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .out_links()
                .ids(),
        );
    }

    #[test]
    fn test_of_type_in_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().of_type("SinkProcessor").in_links().ids());
    }
}

mod permutation_link_to_processor {
    use super::*;

    #[test]
    fn test_e_source_processors_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().ids());
    }

    #[test]
    fn test_e_source_processors_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().count());
    }

    #[test]
    fn test_e_source_processors_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().first());
    }

    #[test]
    fn test_e_source_processors_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().exists());
    }

    #[test]
    fn test_e_source_processors_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().nodes());
    }

    #[test]
    fn test_e_target_processors_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().ids());
    }

    #[test]
    fn test_e_target_processors_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().count());
    }

    #[test]
    fn test_e_target_processors_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().first());
    }

    #[test]
    fn test_e_target_processors_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().exists());
    }

    #[test]
    fn test_e_target_processors_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().nodes());
    }

    #[test]
    fn test_e_source_of_type_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .E()
                .source_processors()
                .of_type("SourceProcessor")
                .ids(),
        );
    }

    #[test]
    fn test_e_target_of_type_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .E()
                .target_processors()
                .of_type("SinkProcessor")
                .ids(),
        );
    }

    #[test]
    fn test_e_source_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().downstream().ids());
    }

    #[test]
    fn test_e_target_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().upstream().ids());
    }
}

// =============================================================================
// PART 12: Complex Multi-Step Query Permutations
// =============================================================================

mod complex_permutations {
    use super::*;

    #[test]
    fn test_sources_downstream_of_type_ids() {
        let graph = mixed_type_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .downstream()
                .of_type("H264Encoder")
                .ids(),
        );
    }

    #[test]
    fn test_of_type_downstream_sinks_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .downstream()
                .downstream()
                .sinks()
                .ids(),
        );
    }

    #[test]
    fn test_sinks_upstream_upstream_sources_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .upstream()
                .upstream()
                .sources()
                .ids(),
        );
    }

    #[test]
    fn test_filter_downstream_filter_upstream_ids() {
        let graph = long_chain_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id == "B")
                .downstream()
                .of_type("ProcessorType2")
                .upstream()
                .ids(),
        );
    }

    #[test]
    fn test_deep_chain_filter_at_each_step() {
        let graph = long_chain_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .filter(|n| n.id.len() == 1)
                .downstream()
                .of_type("ProcessorType2")
                .downstream()
                .has_field("type")
                .downstream()
                .ids(),
        );
    }

    #[test]
    fn test_out_links_target_downstream_out_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .V_from(vec![pid("A")])
                .out_links()
                .target_processors()
                .downstream()
                .out_links()
                .ids(),
        );
    }

    #[test]
    fn test_in_links_source_upstream_in_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .V_from(vec![pid("C")])
                .in_links()
                .source_processors()
                .upstream()
                .in_links()
                .ids(),
        );
    }

    #[test]
    fn test_alternating_downstream_upstream() {
        let graph = diamond_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .downstream()
                .upstream()
                .upstream()
                .ids(),
        );
    }

    #[test]
    fn test_all_filters_combined() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Running)
                .has_field("type")
                .where_field("type", |v| v.as_str().is_some())
                .filter(|n| !n.id.is_empty())
                .ids(),
        );
    }

    #[test]
    fn test_sources_then_every_filter() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Running)
                .filter(|_| true)
                .has_field("type")
                .ids(),
        );
    }

    #[test]
    fn test_sinks_then_every_filter() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Idle)
                .filter(|_| true)
                .has_field("type")
                .ids(),
        );
    }
}

// =============================================================================
// PART 13: All Graph Types × Key Queries
// =============================================================================

mod all_graphs_key_queries {
    use super::*;

    #[test]
    fn test_v_ids_empty() {
        let _ = empty_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_single() {
        let _ = single_processor_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_linear() {
        let _ = linear_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_complex() {
        let _ = complex_graph().execute(&Query::build().v().ids());
    }
    #[test]
    fn test_v_ids_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().ids());
    }

    #[test]
    fn test_sources_empty() {
        let _ = empty_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_single() {
        let _ = single_processor_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_linear() {
        let _ = linear_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_complex() {
        let _ = complex_graph().execute(&Query::build().v().sources().ids());
    }
    #[test]
    fn test_sources_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().sources().ids());
    }

    #[test]
    fn test_sinks_empty() {
        let _ = empty_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_single() {
        let _ = single_processor_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_linear() {
        let _ = linear_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_complex() {
        let _ = complex_graph().execute(&Query::build().v().sinks().ids());
    }
    #[test]
    fn test_sinks_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().sinks().ids());
    }

    #[test]
    fn test_e_ids_empty() {
        let _ = empty_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_single() {
        let _ = single_processor_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_linear() {
        let _ = linear_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_diamond() {
        let _ = diamond_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_mixed() {
        let _ = mixed_type_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_complex() {
        let _ = complex_graph().execute_link(&Query::build().E().ids());
    }
    #[test]
    fn test_e_ids_chain() {
        let _ = long_chain_graph().execute_link(&Query::build().E().ids());
    }
}

// =============================================================================
// PART 14: Additional Permutation Tests (filters × traversals × terminals)
// =============================================================================

mod additional_filter_permutations {
    use super::*;

    // in_state × terminals
    #[test]
    fn test_in_state_running_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Running).ids());
    }
    #[test]
    fn test_in_state_running_count() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Running).count());
    }
    #[test]
    fn test_in_state_running_first() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Running).first());
    }
    #[test]
    fn test_in_state_running_exists() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .exists(),
        );
    }
    #[test]
    fn test_in_state_running_nodes() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Running).nodes());
    }
    #[test]
    fn test_in_state_idle_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Idle).ids());
    }
    #[test]
    fn test_in_state_idle_count() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Idle).count());
    }
    #[test]
    fn test_in_state_stopped_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(&Query::build().v().in_state(ProcessorState::Stopped).ids());
    }

    // in_state × traversals × terminals
    #[test]
    fn test_in_state_downstream_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_in_state_downstream_count() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .downstream()
                .count(),
        );
    }
    #[test]
    fn test_in_state_upstream_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Idle)
                .upstream()
                .ids(),
        );
    }

    // filter closure × terminals
    #[test]
    fn test_filter_closure_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().filter(|n| n.id.starts_with("A")).ids());
    }
    #[test]
    fn test_filter_closure_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().filter(|n| n.id.starts_with("A")).count());
    }
    #[test]
    fn test_filter_closure_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().filter(|n| n.id.starts_with("A")).first());
    }
    #[test]
    fn test_filter_closure_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id.starts_with("A"))
                .exists(),
        );
    }
    #[test]
    fn test_filter_closure_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().filter(|n| n.id.starts_with("A")).nodes());
    }

    // filter × traversals × terminals
    #[test]
    fn test_filter_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id == "A")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_filter_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id == "A")
                .downstream()
                .count(),
        );
    }
    #[test]
    fn test_filter_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().filter(|n| n.id == "C").upstream().ids());
    }
    #[test]
    fn test_filter_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| n.id == "C")
                .upstream()
                .count(),
        );
    }

    // has_field × traversals × terminals
    #[test]
    fn test_has_field_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").downstream().ids());
    }
    #[test]
    fn test_has_field_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").downstream().count());
    }
    #[test]
    fn test_has_field_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").upstream().ids());
    }
    #[test]
    fn test_has_field_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("type").upstream().count());
    }

    // where_field × terminals
    #[test]
    fn test_where_field_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .ids(),
        );
    }
    #[test]
    fn test_where_field_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .count(),
        );
    }
    #[test]
    fn test_where_field_first() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .first(),
        );
    }
    #[test]
    fn test_where_field_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .exists(),
        );
    }
    #[test]
    fn test_where_field_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .nodes(),
        );
    }

    // where_field × traversals × terminals
    #[test]
    fn test_where_field_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("SourceProcessor"))
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_where_field_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("SourceProcessor"))
                .downstream()
                .count(),
        );
    }
    #[test]
    fn test_where_field_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("SinkProcessor"))
                .upstream()
                .ids(),
        );
    }
}

mod additional_link_permutations {
    use super::*;

    // E() × where_field × terminals on all graph types
    #[test]
    fn test_e_where_field_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str().is_some())
                .ids(),
        );
    }
    #[test]
    fn test_e_where_field_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str().is_some())
                .ids(),
        );
    }
    #[test]
    fn test_e_where_field_complex() {
        let graph = complex_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str().is_some())
                .ids(),
        );
    }
    #[test]
    fn test_e_where_field_mixed() {
        let graph = mixed_type_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str().is_some())
                .ids(),
        );
    }

    // E() × has_field × terminals on all graph types
    #[test]
    fn test_e_has_field_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
    }
    #[test]
    fn test_e_has_field_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
    }
    #[test]
    fn test_e_has_field_complex() {
        let graph = complex_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
    }
    #[test]
    fn test_e_has_field_mixed() {
        let graph = mixed_type_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("from.processor").ids());
    }

    // out_links × various terminals
    #[test]
    fn test_out_links_count_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().count());
    }
    #[test]
    fn test_out_links_count_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().count());
    }
    #[test]
    fn test_out_links_first_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().first());
    }
    #[test]
    fn test_out_links_exists_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().exists());
    }
    #[test]
    fn test_out_links_links_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().out_links().links());
    }

    // in_links × various terminals
    #[test]
    fn test_in_links_count_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().count());
    }
    #[test]
    fn test_in_links_count_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().count());
    }
    #[test]
    fn test_in_links_first_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().first());
    }
    #[test]
    fn test_in_links_exists_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().exists());
    }
    #[test]
    fn test_in_links_links_linear() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().v().in_links().links());
    }

    // source_processors × terminals on all graph types
    #[test]
    fn test_source_processors_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute(&Query::build().E().source_processors().ids());
    }
    #[test]
    fn test_source_processors_complex() {
        let graph = complex_graph();
        let _ = graph.execute(&Query::build().E().source_processors().ids());
    }
    #[test]
    fn test_source_processors_mixed() {
        let graph = mixed_type_graph();
        let _ = graph.execute(&Query::build().E().source_processors().ids());
    }
    #[test]
    fn test_source_processors_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().count());
    }
    #[test]
    fn test_source_processors_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().first());
    }
    #[test]
    fn test_source_processors_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().exists());
    }
    #[test]
    fn test_source_processors_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().source_processors().nodes());
    }

    // target_processors × terminals on all graph types
    #[test]
    fn test_target_processors_diamond() {
        let graph = diamond_graph();
        let _ = graph.execute(&Query::build().E().target_processors().ids());
    }
    #[test]
    fn test_target_processors_complex() {
        let graph = complex_graph();
        let _ = graph.execute(&Query::build().E().target_processors().ids());
    }
    #[test]
    fn test_target_processors_mixed() {
        let graph = mixed_type_graph();
        let _ = graph.execute(&Query::build().E().target_processors().ids());
    }
    #[test]
    fn test_target_processors_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().count());
    }
    #[test]
    fn test_target_processors_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().first());
    }
    #[test]
    fn test_target_processors_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().exists());
    }
    #[test]
    fn test_target_processors_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().E().target_processors().nodes());
    }
}

// =============================================================================
// PART 15: Three-Filter Combinations
// =============================================================================

mod three_filter_combinations {
    use super::*;

    // of_type + sources + downstream
    #[test]
    fn test_of_type_sources_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_sources_of_type_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .of_type("SourceProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_sources_downstream_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .downstream()
                .of_type("PassthroughProcessor")
                .ids(),
        );
    }

    // of_type + sinks + upstream
    #[test]
    fn test_of_type_sinks_upstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SinkProcessor")
                .sinks()
                .upstream()
                .ids(),
        );
    }
    #[test]
    fn test_sinks_of_type_upstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("SinkProcessor")
                .upstream()
                .ids(),
        );
    }
    #[test]
    fn test_sinks_upstream_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .upstream()
                .of_type("PassthroughProcessor")
                .ids(),
        );
    }

    // has_field + of_type + downstream
    #[test]
    fn test_has_field_of_type_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .has_field("type")
                .of_type("SourceProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_of_type_has_field_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .has_field("type")
                .downstream()
                .ids(),
        );
    }

    // where_field + of_type + downstream
    #[test]
    fn test_where_field_of_type_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str().is_some())
                .of_type("SourceProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_of_type_where_field_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .where_field("type", |v| v.as_str().is_some())
                .downstream()
                .ids(),
        );
    }

    // filter + sources + downstream
    #[test]
    fn test_filter_sources_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| !n.id.is_empty())
                .sources()
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_sources_filter_downstream() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .filter(|n| !n.id.is_empty())
                .downstream()
                .ids(),
        );
    }

    // in_state + of_type + downstream
    #[test]
    fn test_in_state_of_type_downstream() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .of_type("TestProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_of_type_in_state_downstream() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("TestProcessor")
                .in_state(ProcessorState::Running)
                .downstream()
                .ids(),
        );
    }

    // sources + sinks + of_type (interesting edge case - isolated nodes)
    #[test]
    fn test_sources_sinks_of_type() {
        let graph = single_processor_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .sinks()
                .of_type("TestProcessor")
                .ids(),
        );
    }
}

// =============================================================================
// PART 16: V_from with Different Starting Sets
// =============================================================================

mod v_from_combinations {
    use super::*;

    // V_from with single ID × all filters
    #[test]
    fn test_v_from_single_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .of_type("SourceProcessor")
                .ids(),
        );
    }
    #[test]
    fn test_v_from_single_sources() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).sources().ids());
    }
    #[test]
    fn test_v_from_single_sinks() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("C")]).sinks().ids());
    }
    #[test]
    fn test_v_from_single_has_field() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .has_field("type")
                .ids(),
        );
    }
    #[test]
    fn test_v_from_single_filter() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .filter(|n| !n.id.is_empty())
                .ids(),
        );
    }

    // V_from with multiple IDs × all filters
    #[test]
    fn test_v_from_multi_of_type() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("B"), pid("C")])
                .of_type("SourceProcessor")
                .ids(),
        );
    }
    #[test]
    fn test_v_from_multi_sources() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("B"), pid("C")])
                .sources()
                .ids(),
        );
    }
    #[test]
    fn test_v_from_multi_sinks() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("B"), pid("C")])
                .sinks()
                .ids(),
        );
    }
    #[test]
    fn test_v_from_multi_has_field() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("B"), pid("C")])
                .has_field("type")
                .ids(),
        );
    }
    #[test]
    fn test_v_from_multi_filter() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A"), pid("B"), pid("C")])
                .filter(|n| !n.id.is_empty())
                .ids(),
        );
    }

    // V_from × traversals × terminals
    #[test]
    fn test_v_from_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
    }
    #[test]
    fn test_v_from_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().count());
    }
    #[test]
    fn test_v_from_downstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().first());
    }
    #[test]
    fn test_v_from_downstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().exists());
    }
    #[test]
    fn test_v_from_downstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().nodes());
    }
    #[test]
    fn test_v_from_upstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("C")]).upstream().ids());
    }
    #[test]
    fn test_v_from_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().V_from(vec![pid("C")]).upstream().count());
    }

    // V_from with links
    #[test]
    fn test_v_from_out_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().V_from(vec![pid("A")]).out_links().ids());
    }
    #[test]
    fn test_v_from_out_links_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().V_from(vec![pid("A")]).out_links().count());
    }
    #[test]
    fn test_v_from_in_links_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().V_from(vec![pid("C")]).in_links().ids());
    }
    #[test]
    fn test_v_from_in_links_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().V_from(vec![pid("C")]).in_links().count());
    }
}

// =============================================================================
// PART 17: Graph Type × Query Type Matrix
// =============================================================================

mod graph_type_query_matrix {
    use super::*;

    // downstream × all graph types
    #[test]
    fn test_downstream_empty() {
        let _ = empty_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_single() {
        let _ = single_processor_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_linear() {
        let _ = linear_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_complex() {
        let _ = complex_graph().execute(&Query::build().v().downstream().ids());
    }
    #[test]
    fn test_downstream_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().downstream().ids());
    }

    // upstream × all graph types
    #[test]
    fn test_upstream_empty() {
        let _ = empty_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_single() {
        let _ = single_processor_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_linear() {
        let _ = linear_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_complex() {
        let _ = complex_graph().execute(&Query::build().v().upstream().ids());
    }
    #[test]
    fn test_upstream_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().upstream().ids());
    }

    // of_type × all graph types
    #[test]
    fn test_of_type_empty() {
        let _ = empty_graph().execute(&Query::build().v().of_type("SourceProcessor").ids());
    }
    #[test]
    fn test_of_type_single() {
        let _ =
            single_processor_graph().execute(&Query::build().v().of_type("TestProcessor").ids());
    }
    #[test]
    fn test_of_type_linear() {
        let _ = linear_graph().execute(&Query::build().v().of_type("SourceProcessor").ids());
    }
    #[test]
    fn test_of_type_diamond() {
        let _ = diamond_graph().execute(&Query::build().v().of_type("SourceProcessor").ids());
    }
    #[test]
    fn test_of_type_mixed() {
        let _ = mixed_type_graph().execute(&Query::build().v().of_type("CameraProcessor").ids());
    }
    #[test]
    fn test_of_type_complex() {
        let _ = complex_graph().execute(&Query::build().v().of_type("SourceProcessor").ids());
    }
    #[test]
    fn test_of_type_chain() {
        let _ = long_chain_graph().execute(&Query::build().v().of_type("SourceProcessor").ids());
    }

    // out_links × all graph types
    #[test]
    fn test_out_links_empty() {
        let _ = empty_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_single() {
        let _ = single_processor_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_linear() {
        let _ = linear_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_diamond() {
        let _ = diamond_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_mixed() {
        let _ = mixed_type_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_complex() {
        let _ = complex_graph().execute_link(&Query::build().v().out_links().ids());
    }
    #[test]
    fn test_out_links_chain() {
        let _ = long_chain_graph().execute_link(&Query::build().v().out_links().ids());
    }

    // in_links × all graph types
    #[test]
    fn test_in_links_empty() {
        let _ = empty_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_single() {
        let _ = single_processor_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_linear() {
        let _ = linear_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_diamond() {
        let _ = diamond_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_mixed() {
        let _ = mixed_type_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_complex() {
        let _ = complex_graph().execute_link(&Query::build().v().in_links().ids());
    }
    #[test]
    fn test_in_links_chain() {
        let _ = long_chain_graph().execute_link(&Query::build().v().in_links().ids());
    }
}

// =============================================================================
// PART 18: Additional Edge Cases and Boundary Tests
// =============================================================================

mod additional_edge_cases {
    use super::*;

    // Empty string and special characters
    #[test]
    fn test_of_type_empty_string() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("").ids());
        assert!(result.is_empty());
    }
    #[test]
    fn test_has_field_empty_path() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().has_field("").ids());
    }

    // Chained traversals reaching end of graph
    #[test]
    fn test_downstream_from_sink() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("C")]).downstream().ids());
        assert!(result.is_empty());
    }
    #[test]
    fn test_upstream_from_source() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().V_from(vec![pid("A")]).upstream().ids());
        assert!(result.is_empty());
    }

    // Multiple filters that eliminate all results
    #[test]
    fn test_conflicting_of_type_filters() {
        let graph = linear_graph();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .of_type("SinkProcessor")
                .ids(),
        );
        assert!(result.is_empty());
    }
    #[test]
    fn test_sources_and_sinks_in_connected_graph() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().sources().sinks().ids());
        assert!(result.is_empty()); // A is source but not sink, C is sink but not source
    }

    // exists() on empty results
    #[test]
    fn test_exists_empty_result() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").exists());
        assert!(!result);
    }
    #[test]
    fn test_exists_non_empty_result() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").exists());
        assert!(result);
    }

    // count() on various scenarios
    #[test]
    fn test_count_empty() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").count());
        assert_eq!(result, 0);
    }
    #[test]
    fn test_count_single() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").count());
        assert_eq!(result, 1);
    }
    #[test]
    fn test_count_multiple() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 3);
    }

    // first() on various scenarios
    #[test]
    fn test_first_empty() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("NonExistent").first());
        assert!(result.is_none());
    }
    #[test]
    fn test_first_non_empty() {
        let graph = linear_graph();
        let result = graph.execute(&Query::build().v().of_type("SourceProcessor").first());
        assert!(result.is_some());
    }

    // All terminals on empty graph
    #[test]
    fn test_empty_graph_ids() {
        let result = empty_graph().execute(&Query::build().v().ids());
        assert!(result.is_empty());
    }
    #[test]
    fn test_empty_graph_count() {
        let result = empty_graph().execute(&Query::build().v().count());
        assert_eq!(result, 0);
    }
    #[test]
    fn test_empty_graph_first() {
        let result = empty_graph().execute(&Query::build().v().first());
        assert!(result.is_none());
    }
    #[test]
    fn test_empty_graph_exists() {
        let result = empty_graph().execute(&Query::build().v().exists());
        assert!(!result);
    }
    #[test]
    fn test_empty_graph_nodes() {
        let result = empty_graph().execute(&Query::build().v().nodes());
        assert!(result.is_empty());
    }

    // Link terminals on empty graph
    #[test]
    fn test_empty_graph_link_ids() {
        let result = empty_graph().execute_link(&Query::build().E().ids());
        assert!(result.is_empty());
    }
    #[test]
    fn test_empty_graph_link_count() {
        let result = empty_graph().execute_link(&Query::build().E().count());
        assert_eq!(result, 0);
    }
    #[test]
    fn test_empty_graph_link_first() {
        let result = empty_graph().execute_link(&Query::build().E().first());
        assert!(result.is_none());
    }
    #[test]
    fn test_empty_graph_link_exists() {
        let result = empty_graph().execute_link(&Query::build().E().exists());
        assert!(!result);
    }
    #[test]
    fn test_empty_graph_link_links() {
        let result = empty_graph().execute_link(&Query::build().E().links());
        assert!(result.is_empty());
    }
}

// =============================================================================
// PART 19: Filter + Filter + Traversal + Terminal Permutations
// =============================================================================

mod four_step_permutations {
    use super::*;

    // of_type + sources + downstream + various terminals
    #[test]
    fn test_of_type_sources_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .downstream()
                .count(),
        );
    }
    #[test]
    fn test_of_type_sources_downstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .downstream()
                .first(),
        );
    }
    #[test]
    fn test_of_type_sources_downstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .downstream()
                .exists(),
        );
    }
    #[test]
    fn test_of_type_sources_downstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .sources()
                .downstream()
                .nodes(),
        );
    }

    // sinks + of_type + upstream + various terminals
    #[test]
    fn test_sinks_of_type_upstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("SinkProcessor")
                .upstream()
                .count(),
        );
    }
    #[test]
    fn test_sinks_of_type_upstream_first() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("SinkProcessor")
                .upstream()
                .first(),
        );
    }
    #[test]
    fn test_sinks_of_type_upstream_exists() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("SinkProcessor")
                .upstream()
                .exists(),
        );
    }
    #[test]
    fn test_sinks_of_type_upstream_nodes() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .of_type("SinkProcessor")
                .upstream()
                .nodes(),
        );
    }

    // has_field + where_field + downstream + terminals
    #[test]
    fn test_has_where_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .has_field("type")
                .where_field("type", |v| v.as_str().is_some())
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_has_where_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .has_field("type")
                .where_field("type", |v| v.as_str().is_some())
                .downstream()
                .count(),
        );
    }

    // filter + of_type + downstream + terminals
    #[test]
    fn test_filter_of_type_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| !n.id.is_empty())
                .of_type("SourceProcessor")
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_filter_of_type_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .filter(|n| !n.id.is_empty())
                .of_type("SourceProcessor")
                .downstream()
                .count(),
        );
    }

    // V_from + filter + downstream + terminals
    #[test]
    fn test_v_from_filter_downstream_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .filter(|n| !n.id.is_empty())
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_v_from_filter_downstream_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .filter(|n| !n.id.is_empty())
                .downstream()
                .count(),
        );
    }

    // in_state + filter + downstream + terminals
    #[test]
    fn test_in_state_filter_downstream_ids() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .filter(|n| !n.id.is_empty())
                .downstream()
                .ids(),
        );
    }
    #[test]
    fn test_in_state_filter_downstream_count() {
        let graph = graph_with_states();
        let _ = graph.execute(
            &Query::build()
                .v()
                .in_state(ProcessorState::Running)
                .filter(|n| !n.id.is_empty())
                .downstream()
                .count(),
        );
    }
}

// =============================================================================
// PART 20: Link Query Filter + Terminal Matrix
// =============================================================================

mod link_filter_terminal_matrix {
    use super::*;

    // where_field × all terminals
    #[test]
    fn test_link_where_field_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .ids(),
        );
    }
    #[test]
    fn test_link_where_field_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .count(),
        );
    }
    #[test]
    fn test_link_where_field_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .first(),
        );
    }
    #[test]
    fn test_link_where_field_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .exists(),
        );
    }
    #[test]
    fn test_link_where_field_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .links(),
        );
    }

    // has_field × all terminals
    #[test]
    fn test_link_has_field_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("to.processor").ids());
    }
    #[test]
    fn test_link_has_field_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("to.processor").count());
    }
    #[test]
    fn test_link_has_field_first() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("to.processor").first());
    }
    #[test]
    fn test_link_has_field_exists() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("to.processor").exists());
    }
    #[test]
    fn test_link_has_field_links() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().has_field("to.processor").links());
    }

    // where_field + has_field combined
    #[test]
    fn test_link_where_has_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str().is_some())
                .has_field("to.processor")
                .ids(),
        );
    }
    #[test]
    fn test_link_has_where_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(
            &Query::build()
                .E()
                .has_field("from.processor")
                .where_field("to.processor", |v| v.as_str().is_some())
                .ids(),
        );
    }
}

// =============================================================================
// PART 21: Cross-Query Permutations (Processor → Link → Processor)
// =============================================================================

mod cross_query_permutations {
    use super::*;

    // v().out_links().source_processors() variations
    #[test]
    fn test_v_out_source_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().out_links().source_processors().ids());
    }
    #[test]
    fn test_v_out_source_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().out_links().source_processors().count());
    }
    #[test]
    fn test_v_out_target_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().out_links().target_processors().ids());
    }
    #[test]
    fn test_v_out_target_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().out_links().target_processors().count());
    }

    // v().in_links().source_processors() variations
    #[test]
    fn test_v_in_source_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().in_links().source_processors().ids());
    }
    #[test]
    fn test_v_in_source_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().in_links().source_processors().count());
    }
    #[test]
    fn test_v_in_target_ids() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().in_links().target_processors().ids());
    }
    #[test]
    fn test_v_in_target_count() {
        let graph = linear_graph();
        let _ = graph.execute(&Query::build().v().in_links().target_processors().count());
    }

    // sources().out_links().target_processors() chain
    #[test]
    fn test_sources_out_target_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .out_links()
                .target_processors()
                .ids(),
        );
    }
    #[test]
    fn test_sources_out_target_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sources()
                .out_links()
                .target_processors()
                .count(),
        );
    }

    // sinks().in_links().source_processors() chain
    #[test]
    fn test_sinks_in_source_ids() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .in_links()
                .source_processors()
                .ids(),
        );
    }
    #[test]
    fn test_sinks_in_source_count() {
        let graph = linear_graph();
        let _ = graph.execute(
            &Query::build()
                .v()
                .sinks()
                .in_links()
                .source_processors()
                .count(),
        );
    }

    // E().source_processors().out_links() chain
    #[test]
    fn test_e_source_out_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().source_processors().out_links().ids());
    }
    #[test]
    fn test_e_source_out_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().source_processors().out_links().count());
    }

    // E().target_processors().in_links() chain
    #[test]
    fn test_e_target_in_ids() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().target_processors().in_links().ids());
    }
    #[test]
    fn test_e_target_in_count() {
        let graph = linear_graph();
        let _ = graph.execute_link(&Query::build().E().target_processors().in_links().count());
    }
}

// =============================================================================
// PART 22: JSON Field Queryability Tests
// =============================================================================
// Verifies that all fields produced in processor/link JSON are queryable

mod json_field_queryability {
    use super::*;
    use crate::core::graph::components::ProcessorMetrics;

    /// Create a graph with full ECS components attached
    fn graph_with_ecs_components() -> Graph {
        let mut graph = Graph::new();

        // Add processors
        graph.add_processor("A".into(), "SourceProcessor".into(), 0);
        graph.add_processor("B".into(), "ProcessorMiddle".into(), 0);
        graph.add_processor("C".into(), "SinkProcessor".into(), 0);

        // Add links
        graph.add_link("A.output", "B.input").unwrap();
        graph.add_link("B.output", "C.input").unwrap();

        // Attach ECS StateComponent
        graph
            .insert(&pid("A"), state(ProcessorState::Running))
            .unwrap();
        graph
            .insert(&pid("B"), state(ProcessorState::Running))
            .unwrap();
        graph
            .insert(&pid("C"), state(ProcessorState::Idle))
            .unwrap();

        // Attach ECS ProcessorMetrics
        let metrics = ProcessorMetrics {
            throughput_fps: 30.0,
            latency_p50_ms: 5.0,
            latency_p99_ms: 15.0,
            frames_processed: 1000,
            frames_dropped: 2,
        };
        graph.insert(&pid("A"), metrics.clone()).unwrap();
        graph.insert(&pid("B"), metrics).unwrap();

        graph
    }

    // --- Processor JSON Fields ---

    #[test]
    fn test_query_processor_type_field() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("type", |v| v.as_str() == Some("SourceProcessor"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_query_processor_type_has_field() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(&Query::build().v().has_field("type").count());
        assert_eq!(result, 3); // All processors have type
    }

    #[test]
    fn test_query_processor_state_field() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("state", |v| v.as_str() == Some("Running"))
                .ids(),
        );
        assert_eq!(result.len(), 2); // A and B are Running
    }

    #[test]
    fn test_query_processor_state_idle() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("state", |v| v.as_str() == Some("Idle"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("C")));
    }

    #[test]
    fn test_query_processor_metrics_throughput() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.throughput_fps", |v| {
                    v.as_f64().map(|f| f > 25.0).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2); // A and B have metrics
    }

    #[test]
    fn test_query_processor_metrics_latency_p50() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.latency_p50_ms", |v| {
                    v.as_f64().map(|f| f < 10.0).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_query_processor_metrics_latency_p99() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.latency_p99_ms", |v| {
                    v.as_f64().map(|f| f > 10.0).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_query_processor_metrics_frames_processed() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.frames_processed", |v| {
                    v.as_u64().map(|n| n >= 1000).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_query_processor_metrics_frames_dropped() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.frames_dropped", |v| {
                    v.as_u64().map(|n| n < 5).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_query_processor_has_metrics() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(&Query::build().v().has_field("metrics").count());
        assert_eq!(result, 2); // Only A and B have metrics
    }

    #[test]
    fn test_query_processor_missing_metrics() {
        let graph = graph_with_ecs_components();
        // C doesn't have metrics
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("metrics.throughput_fps", |_| true)
                .ids(),
        );
        assert!(!result.contains(&pid("C")));
    }

    // --- Link JSON Fields ---

    #[test]
    fn test_query_link_from_processor() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_query_link_from_port() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.port", |v| v.as_str() == Some("output"))
                .ids(),
        );
        assert_eq!(result.len(), 2); // Both links have "output" port
    }

    #[test]
    fn test_query_link_to_processor() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("to.processor", |v| v.as_str() == Some("C"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_query_link_to_port() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(
            &Query::build()
                .E()
                .where_field("to.port", |v| v.as_str() == Some("input"))
                .ids(),
        );
        assert_eq!(result.len(), 2); // Both links have "input" port
    }

    #[test]
    fn test_query_link_has_from() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(&Query::build().E().has_field("from").count());
        assert_eq!(result, 2); // All links have from
    }

    #[test]
    fn test_query_link_has_to() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(&Query::build().E().has_field("to").count());
        assert_eq!(result, 2); // All links have to
    }

    #[test]
    fn test_query_link_has_from_processor() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(&Query::build().E().has_field("from.processor").count());
        assert_eq!(result, 2);
    }

    #[test]
    fn test_query_link_has_to_port() {
        let graph = graph_with_ecs_components();
        let result = graph.execute_link(&Query::build().E().has_field("to.port").count());
        assert_eq!(result, 2);
    }

    // --- Combined processor field queries ---

    #[test]
    fn test_query_running_with_high_throughput() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .where_field("state", |v| v.as_str() == Some("Running"))
                .where_field("metrics.throughput_fps", |v| {
                    v.as_f64().map(|f| f > 20.0).unwrap_or(false)
                })
                .ids(),
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_query_type_and_state() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .v()
                .of_type("SourceProcessor")
                .where_field("state", |v| v.as_str() == Some("Running"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("A")));
    }

    #[test]
    fn test_query_downstream_running_processors() {
        let graph = graph_with_ecs_components();
        let result = graph.execute(
            &Query::build()
                .V_from(vec![pid("A")])
                .downstream()
                .where_field("state", |v| v.as_str() == Some("Running"))
                .ids(),
        );
        assert_eq!(result.len(), 1);
        assert!(result.contains(&pid("B")));
    }
}

// =============================================================================
// PART 23: Dynamic Graph Modification Tests
// =============================================================================
// Tests that queries update correctly when processors/links are added/removed

mod dynamic_graph_modification {
    use super::*;

    #[test]
    fn test_add_processor_updates_query() {
        let mut graph = Graph::new();

        // Initially empty
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 0);

        // Add first processor
        graph.add_processor("A".into(), "TestProcessor".into(), 0);
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 1);

        // Add second processor
        graph.add_processor("B".into(), "TestProcessor".into(), 0);
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 2);

        // Add third processor of different type
        graph.add_processor("C".into(), "OtherProcessor".into(), 0);
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 3);

        // Type filter should update
        let result = graph.execute(&Query::build().v().of_type("TestProcessor").count());
        assert_eq!(result, 2);

        let result = graph.execute(&Query::build().v().of_type("OtherProcessor").count());
        assert_eq!(result, 1);
    }

    #[test]
    fn test_remove_processor_updates_query() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "TestProcessor".into(), 0);
        graph.add_processor("B".into(), "TestProcessor".into(), 0);
        graph.add_processor("C".into(), "TestProcessor".into(), 0);

        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 3);

        // Remove processor
        graph.remove_processor(&pid("B"));
        let result = graph.execute(&Query::build().v().count());
        assert_eq!(result, 2);

        // Verify correct processors remain
        let ids = graph.execute(&Query::build().v().ids());
        assert!(ids.contains(&pid("A")));
        assert!(!ids.contains(&pid("B")));
        assert!(ids.contains(&pid("C")));
    }

    #[test]
    fn test_add_link_updates_query() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "SourceProcessor".into(), 0);
        graph.add_processor("B".into(), "MiddleProcessor".into(), 0);
        graph.add_processor("C".into(), "SinkProcessor".into(), 0);

        // Initially all are sources and sinks (no links)
        let sources = graph.execute(&Query::build().v().sources().count());
        let sinks = graph.execute(&Query::build().v().sinks().count());
        assert_eq!(sources, 3);
        assert_eq!(sinks, 3);

        // Add first link
        graph.add_link("A.out", "B.in").unwrap();

        let sources = graph.execute(&Query::build().v().sources().count());
        let sinks = graph.execute(&Query::build().v().sinks().count());
        assert_eq!(sources, 2); // A and C are sources
        assert_eq!(sinks, 2); // B and C are sinks

        // Downstream query should work
        let downstream = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream.len(), 1);
        assert!(downstream.contains(&pid("B")));

        // Add second link
        graph.add_link("B.out", "C.in").unwrap();

        let sources = graph.execute(&Query::build().v().sources().count());
        let sinks = graph.execute(&Query::build().v().sinks().count());
        assert_eq!(sources, 1); // Only A
        assert_eq!(sinks, 1); // Only C

        // Downstream from A returns direct neighbors only (B, not C)
        let downstream = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream.len(), 1);
        assert!(downstream.contains(&pid("B")));

        // Downstream from B should include C
        let downstream_b = graph.execute(&Query::build().V_from(vec![pid("B")]).downstream().ids());
        assert_eq!(downstream_b.len(), 1);
        assert!(downstream_b.contains(&pid("C")));
    }

    #[test]
    fn test_remove_link_updates_query() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "SourceProcessor".into(), 0);
        graph.add_processor("B".into(), "MiddleProcessor".into(), 0);
        graph.add_processor("C".into(), "SinkProcessor".into(), 0);
        let _link_ab = graph.add_link("A.out", "B.in").unwrap();
        let link_bc = graph.add_link("B.out", "C.in").unwrap();

        // Verify initial state
        let link_count = graph.execute_link(&Query::build().E().count());
        assert_eq!(link_count, 2);

        let sources = graph.execute(&Query::build().v().sources().count());
        assert_eq!(sources, 1);

        // Remove second link using its LinkId
        graph.remove_link(&link_bc.id);

        let link_count = graph.execute_link(&Query::build().E().count());
        assert_eq!(link_count, 1);

        // C is now a source (no incoming links)
        let sources = graph.execute(&Query::build().v().sources().count());
        assert_eq!(sources, 2); // A and C

        // Downstream from A should only include B now
        let downstream = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream.len(), 1);
        assert!(downstream.contains(&pid("B")));
    }

    #[test]
    fn test_multiple_add_remove_link_cycles() {
        // Test adding and removing links (without processor removal which has known issues)
        let mut graph = Graph::new();

        // Setup: Build a 3-node pipeline
        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "Middle".into(), 0);
        graph.add_processor("C".into(), "Sink".into(), 0);

        // Initially all are sources and sinks (no links)
        assert_eq!(graph.execute(&Query::build().v().count()), 3);
        assert_eq!(graph.execute_link(&Query::build().E().count()), 0);
        assert_eq!(graph.execute(&Query::build().v().sources().count()), 3);
        assert_eq!(graph.execute(&Query::build().v().sinks().count()), 3);

        // Cycle 1: Add first link A -> B
        let link_ab = graph.add_link("A.out", "B.in").unwrap();
        assert_eq!(graph.execute_link(&Query::build().E().count()), 1);
        assert_eq!(graph.execute(&Query::build().v().sources().count()), 2); // A, C
        assert_eq!(graph.execute(&Query::build().v().sinks().count()), 2); // B, C

        // Cycle 2: Add second link B -> C
        let link_bc = graph.add_link("B.out", "C.in").unwrap();
        assert_eq!(graph.execute_link(&Query::build().E().count()), 2);
        assert_eq!(graph.execute(&Query::build().v().sources().count()), 1); // A only
        assert_eq!(graph.execute(&Query::build().v().sinks().count()), 1); // C only

        // Cycle 3: Remove link B -> C
        graph.remove_link(&link_bc.id);
        assert_eq!(graph.execute_link(&Query::build().E().count()), 1);
        assert_eq!(graph.execute(&Query::build().v().sources().count()), 2); // A, C
        assert_eq!(graph.execute(&Query::build().v().sinks().count()), 2); // B, C

        // Cycle 4: Remove link A -> B
        graph.remove_link(&link_ab.id);
        assert_eq!(graph.execute_link(&Query::build().E().count()), 0);
        assert_eq!(graph.execute(&Query::build().v().sources().count()), 3);
        assert_eq!(graph.execute(&Query::build().v().sinks().count()), 3);

        // Cycle 5: Add new direct link A -> C
        graph.add_link("A.direct", "C.direct").unwrap();
        assert_eq!(graph.execute_link(&Query::build().E().count()), 1);
        let downstream = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream.len(), 1);
        assert!(downstream.contains(&pid("C")));
    }

    #[test]
    fn test_ecs_component_updates() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "TestProcessor".into(), 0);

        // Initially no state component
        let running = graph.execute(&Query::build().v().in_state(ProcessorState::Running).count());
        assert_eq!(running, 0);

        // Add state component
        graph
            .insert(&pid("A"), state(ProcessorState::Running))
            .unwrap();

        let running = graph.execute(&Query::build().v().in_state(ProcessorState::Running).count());
        assert_eq!(running, 1);

        // Update state by inserting new component
        graph
            .insert(&pid("A"), state(ProcessorState::Idle))
            .unwrap();

        let running = graph.execute(&Query::build().v().in_state(ProcessorState::Running).count());
        let idle = graph.execute(&Query::build().v().in_state(ProcessorState::Idle).count());
        assert_eq!(running, 0);
        assert_eq!(idle, 1);
    }

    #[test]
    fn test_complex_graph_link_reconfiguration() {
        // Test complex link reconfiguration without processor removal
        let mut graph = Graph::new();

        // Phase 1: Diamond topology A -> (B, C) -> D
        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "EncoderH264".into(), 0);
        graph.add_processor("C".into(), "EncoderOpus".into(), 0);
        graph.add_processor("D".into(), "Muxer".into(), 0);
        graph.add_processor("E".into(), "Display".into(), 0);

        let _link_a_b = graph.add_link("A.video", "B.in").unwrap();
        let link_a_c = graph.add_link("A.audio", "C.in").unwrap();
        let _link_b_d = graph.add_link("B.out", "D.video").unwrap();
        let link_c_d = graph.add_link("C.out", "D.audio").unwrap();

        // Verify diamond structure
        let sources = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(sources.len(), 2); // A and E (E not connected yet)
        assert!(sources.contains(&pid("A")));
        assert!(sources.contains(&pid("E")));

        let sinks = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(sinks.len(), 2); // D and E
        assert!(sinks.contains(&pid("D")));
        assert!(sinks.contains(&pid("E")));

        // downstream() returns direct neighbors only: B and C
        let downstream_a = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream_a.len(), 2); // B and C (direct neighbors, not D)
        assert!(downstream_a.contains(&pid("B")));
        assert!(downstream_a.contains(&pid("C")));

        // Phase 2: Add parallel output path to E
        graph.add_link("A.preview", "E.in").unwrap();

        let sinks = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(sinks.len(), 2); // D and E

        let sources = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(sources.len(), 1); // Only A now (E has incoming link)

        // Phase 3: Remove audio path links (C becomes isolated but still exists)
        graph.remove_link(&link_a_c.id);
        graph.remove_link(&link_c_d.id);

        // All processors still exist
        assert_eq!(graph.execute(&Query::build().v().count()), 5);
        // C is now isolated (source and sink)
        let sources = graph.execute(&Query::build().v().sources().ids());
        assert!(sources.contains(&pid("A")));
        assert!(sources.contains(&pid("C"))); // C is now a source (no incoming)

        let sinks = graph.execute(&Query::build().v().sinks().ids());
        assert!(sinks.contains(&pid("C"))); // C is now a sink (no outgoing)
        assert!(sinks.contains(&pid("D")));
        assert!(sinks.contains(&pid("E")));

        // Phase 4: Verify remaining structure
        // A's direct neighbors are now: B (via video), E (via preview)
        let downstream_a = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream_a.len(), 2); // B and E (direct neighbors only)
        assert!(downstream_a.contains(&pid("B")));
        assert!(downstream_a.contains(&pid("E")));
    }

    #[test]
    fn test_query_reuse_after_modification() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "Sink".into(), 0);

        // Create a reusable query
        let sources_query = Query::build().v().sources().ids();

        // Execute before adding link
        let result1 = graph.execute(&sources_query);
        assert_eq!(result1.len(), 2); // Both are sources

        // Add link
        let link = graph.add_link("A.out", "B.in").unwrap();

        // Same query, different result
        let result2 = graph.execute(&sources_query);
        assert_eq!(result2.len(), 1); // Only A is source now

        // Remove link using its LinkId
        graph.remove_link(&link.id);

        // Back to original state
        let result3 = graph.execute(&sources_query);
        assert_eq!(result3.len(), 2);
    }

    #[test]
    fn test_link_query_after_processor_removal() {
        let mut graph = Graph::new();
        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "Middle".into(), 0);
        graph.add_processor("C".into(), "Sink".into(), 0);
        graph.add_link("A.out", "B.in").unwrap();
        graph.add_link("B.out", "C.in").unwrap();

        // Links from A
        let links_from_a = graph.execute_link(
            &Query::build()
                .E()
                .where_field("from.processor", |v| v.as_str() == Some("A"))
                .count(),
        );
        assert_eq!(links_from_a, 1);

        // Remove B (should remove both links)
        graph.remove_processor(&pid("B"));

        let total_links = graph.execute_link(&Query::build().E().count());
        assert_eq!(total_links, 0);
    }

    /// Regression test: removing a processor should not corrupt the graph's
    /// internal index, allowing subsequent operations on remaining processors.
    #[test]
    fn test_processor_removal_then_link_remaining_processors() {
        let mut graph = Graph::new();

        // Add three processors: A, B, C
        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "Middle".into(), 0);
        graph.add_processor("C".into(), "Sink".into(), 0);

        assert_eq!(graph.execute(&Query::build().v().count()), 3);

        // Remove B (the middle one)
        graph.remove_processor(&pid("B"));

        assert_eq!(graph.execute(&Query::build().v().count()), 2);
        assert!(graph.execute(&Query::build().v().ids()).contains(&pid("A")));
        assert!(graph.execute(&Query::build().v().ids()).contains(&pid("C")));

        // This is the critical test: can we still add a link between A and C?
        // With the buggy secondary index, this would panic because C's NodeIndex
        // was invalidated when B was removed.
        let link = graph.add_link("A.out", "C.in").unwrap();

        // Verify the link was created correctly
        assert_eq!(graph.execute_link(&Query::build().E().count()), 1);

        let downstream = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream.len(), 1);
        assert!(downstream.contains(&pid("C")));

        // Also verify we can remove and re-add
        graph.remove_link(&link.id);
        assert_eq!(graph.execute_link(&Query::build().E().count()), 0);

        graph.add_link("A.another", "C.another").unwrap();
        assert_eq!(graph.execute_link(&Query::build().E().count()), 1);
    }

    /// Regression test: multiple processor removals should not corrupt indices.
    #[test]
    fn test_multiple_processor_removals_then_operations() {
        let mut graph = Graph::new();

        // Add five processors
        graph.add_processor("A".into(), "Type1".into(), 0);
        graph.add_processor("B".into(), "Type2".into(), 0);
        graph.add_processor("C".into(), "Type3".into(), 0);
        graph.add_processor("D".into(), "Type4".into(), 0);
        graph.add_processor("E".into(), "Type5".into(), 0);

        // Add some links
        graph.add_link("A.out", "B.in").unwrap();
        graph.add_link("B.out", "C.in").unwrap();
        graph.add_link("C.out", "D.in").unwrap();
        graph.add_link("D.out", "E.in").unwrap();

        assert_eq!(graph.execute(&Query::build().v().count()), 5);
        assert_eq!(graph.execute_link(&Query::build().E().count()), 4);

        // Remove B and D (non-adjacent processors)
        graph.remove_processor(&pid("B"));
        graph.remove_processor(&pid("D"));

        assert_eq!(graph.execute(&Query::build().v().count()), 3);
        // A->B (gone), B->C (gone), C->D (gone), D->E (gone)
        // All links involved B or D, so all should be gone
        assert_eq!(graph.execute_link(&Query::build().E().count()), 0);

        // Now add new links between remaining processors A, C, E
        graph.add_link("A.out", "C.in").unwrap();
        graph.add_link("C.out", "E.in").unwrap();

        assert_eq!(graph.execute_link(&Query::build().E().count()), 2);

        // Verify structure
        let downstream_a = graph.execute(&Query::build().V_from(vec![pid("A")]).downstream().ids());
        assert_eq!(downstream_a.len(), 1);
        assert!(downstream_a.contains(&pid("C")));

        let downstream_c = graph.execute(&Query::build().V_from(vec![pid("C")]).downstream().ids());
        assert_eq!(downstream_c.len(), 1);
        assert!(downstream_c.contains(&pid("E")));
    }

    /// Regression test: remove processor, add new processor, then link them.
    #[test]
    fn test_remove_processor_add_new_then_link() {
        let mut graph = Graph::new();

        graph.add_processor("A".into(), "Source".into(), 0);
        graph.add_processor("B".into(), "ToRemove".into(), 0);
        graph.add_processor("C".into(), "Sink".into(), 0);

        // Remove B
        graph.remove_processor(&pid("B"));

        // Add a new processor D
        graph.add_processor("D".into(), "NewProcessor".into(), 0);

        assert_eq!(graph.execute(&Query::build().v().count()), 3); // A, C, D

        // Link A -> D -> C
        graph.add_link("A.out", "D.in").unwrap();
        graph.add_link("D.out", "C.in").unwrap();

        assert_eq!(graph.execute_link(&Query::build().E().count()), 2);

        // Verify chain
        let sources = graph.execute(&Query::build().v().sources().ids());
        assert_eq!(sources.len(), 1);
        assert!(sources.contains(&pid("A")));

        let sinks = graph.execute(&Query::build().v().sinks().ids());
        assert_eq!(sinks.len(), 1);
        assert!(sinks.contains(&pid("C")));
    }
}
