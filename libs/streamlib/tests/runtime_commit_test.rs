// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Integration tests for the commit-based runtime synchronization flow.
///
/// Tests the Auto/Manual commit modes and delta-based synchronization between
/// Graph (desired state) and ExecutionGraph (running state).
use streamlib::core::processors::{
    ProcessorNodeFactory, RegistryBackedFactory, SimplePassthroughProcessor,
};
use streamlib::core::runtime::{CommitMode, StreamRuntime};

#[test]
fn test_runtime_default_is_auto_commit() {
    // Default runtime uses auto-commit - verify by checking behavior
    let mut runtime = StreamRuntime::new();

    // Add a processor - should be added to graph immediately (auto-commit behavior)
    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    let graph = runtime.graph().read();
    assert!(graph.has_processor(&node));
}

#[test]
fn test_runtime_manual_commit_mode() {
    // Manual mode requires explicit commit - verify by checking behavior
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::Manual)
        .build();

    // Add processor - added to graph but pending operations queued
    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    // Processor exists in graph (topology added immediately)
    let graph = runtime.graph().read();
    assert!(graph.has_processor(&node));
}

#[test]
fn test_auto_commit_syncs_graph_changes() {
    let mut runtime = StreamRuntime::new();

    // Add a processor - should auto-commit
    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    // Verify node was added to graph
    let graph = runtime.graph().read();
    assert!(graph.has_processor(&node));
    assert_eq!(graph.processor_count(), 1);
}

#[test]
fn test_manual_commit_batches_changes() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::Manual)
        .build();

    // Add multiple processors without committing
    let node1 = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor 1");

    let node2 = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor 2");

    // Verify nodes are in the graph
    {
        let graph = runtime.graph().read();
        assert!(graph.has_processor(&node1.id));
        assert!(graph.has_processor(&node2.id));
        assert_eq!(graph.processor_count(), 2);
    }

    // Explicit commit should sync to executor
    runtime.commit().expect("Commit failed");
}

#[test]
fn test_connect_with_auto_commit() {
    let mut runtime = StreamRuntime::new();

    // Add source and sink processors
    let source = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add source");

    let sink = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add sink");

    // Connect them - in auto mode this syncs immediately
    let link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("Failed to connect");

    // Verify link was added to graph
    let graph = runtime.graph().read();
    assert_eq!(graph.link_count(), 1);
    assert!(graph.get_link(&link.id).is_some());
}

#[test]
fn test_disconnect_removes_link() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::Manual)
        .build();

    // Setup: add processors and connect
    let source = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add source");

    let sink = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add sink");

    let link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("Failed to connect");

    // Verify link exists in graph (added immediately to graph structure)
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.link_count(), 1);
    }

    // Disconnect - queues the operation
    runtime.disconnect(&link).expect("Failed to disconnect");

    // Commit before start does nothing - link should still exist
    runtime.commit().expect("Commit before start");
    {
        let graph = runtime.graph().read();
        assert_eq!(
            graph.link_count(),
            1,
            "Link should still exist before start"
        );
    }

    // Start runtime - executes pending operations (including the disconnect)
    runtime.start().expect("Failed to start");

    // Verify link is removed after start
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.link_count(), 0, "Link should be removed after start");
    }

    runtime.stop().expect("Failed to stop");
}

#[test]
fn test_remove_processor() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::Manual)
        .build();

    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    // Verify exists in graph (added immediately to graph structure)
    {
        let graph = runtime.graph().read();
        assert!(graph.has_processor(&node));
    }

    // Remove - queues the operation
    runtime
        .remove_processor(&node)
        .expect("Failed to remove processor");

    // Commit before start does nothing - processor should still exist
    runtime.commit().expect("Commit before start");
    {
        let graph = runtime.graph().read();
        assert!(
            graph.has_processor(&node),
            "Processor should still exist before start"
        );
        assert_eq!(graph.processor_count(), 1);
    }

    // Start runtime - executes pending operations (including the remove)
    runtime.start().expect("Failed to start");

    // Verify removed after start
    {
        let graph = runtime.graph().read();
        assert!(
            !graph.has_processor(&node),
            "Processor should be removed after start"
        );
        assert_eq!(graph.processor_count(), 0);
    }

    runtime.stop().expect("Failed to stop");
}

// Test the factory registration and lookup
#[test]
fn test_registry_factory_registration() {
    let factory = RegistryBackedFactory::new();

    // Before registration
    assert!(!factory.is_registered("SimplePassthroughProcessor"));
    assert!(!factory.can_create("SimplePassthroughProcessor"));

    // Register
    factory.register::<SimplePassthroughProcessor::Processor>();

    // After registration
    assert!(factory.is_registered("SimplePassthroughProcessor"));
    assert!(factory.can_create("SimplePassthroughProcessor"));

    // Port info should be available
    let port_info = factory.port_info("SimplePassthroughProcessor");
    assert!(port_info.is_some());
    let (inputs, outputs) = port_info.unwrap();
    assert!(!inputs.is_empty());
    assert!(!outputs.is_empty());
}

// Test port unwiring with LinkInstance architecture
mod unwire_tests {
    use streamlib::core::links::graph::link_id;
    use streamlib::core::processors::SimplePassthroughProcessor;
    use streamlib::core::runtime::{CommitMode, StreamRuntime};
    use streamlib::core::LinkInput;
    use streamlib::core::LinkInstance;
    use streamlib::core::LinkOutput;
    use streamlib::core::VideoFrame;

    #[test]
    fn test_link_output_add_remove() {
        // Test that LinkOutput properly handles add_data_writer/remove_data_writer
        let output: LinkOutput<VideoFrame> = LinkOutput::new("test_output");

        // Initially disconnected
        assert!(!output.is_connected());
        assert_eq!(output.link_count(), 0);

        // Create a LinkInstance and get data writer
        let link_id = link_id::__private::new_unchecked("test_link".to_string());
        let instance = LinkInstance::<VideoFrame>::new(link_id.clone(), 16);
        let output_data_writer = instance.create_link_output_data_writer();

        // Add data writer
        output
            .add_data_writer(link_id.clone(), output_data_writer)
            .unwrap();
        assert!(output.is_connected());
        assert_eq!(output.link_count(), 1);

        // Remove data writer
        output.remove_data_writer(&link_id).unwrap();
        assert!(!output.is_connected());
        assert_eq!(output.link_count(), 0);
    }

    #[test]
    fn test_link_input_add_remove() {
        // Test that LinkInput properly handles add_data_reader/remove_data_reader
        let input: LinkInput<VideoFrame> = LinkInput::new("test_input");

        // Initially disconnected
        assert!(!input.is_connected());
        assert_eq!(input.link_count(), 0);

        // Create a LinkInstance and get data reader
        let link_id = link_id::__private::new_unchecked("test_link".to_string());
        let instance = LinkInstance::<VideoFrame>::new(link_id.clone(), 16);
        let input_data_reader = instance.create_link_input_data_reader();

        // Add data reader
        input
            .add_data_reader(link_id.clone(), input_data_reader, None)
            .unwrap();
        assert!(input.is_connected());
        assert_eq!(input.link_count(), 1);

        // Remove data reader
        input.remove_data_reader(&link_id).unwrap();
        assert!(!input.is_connected());
        assert_eq!(input.link_count(), 0);
    }

    #[test]
    fn test_link_remove_not_found_error() {
        let output: LinkOutput<VideoFrame> = LinkOutput::new("test_output");
        let link_id = link_id::__private::new_unchecked("nonexistent".to_string());

        // Removing a data writer that doesn't exist should error
        let result = output.remove_data_writer(&link_id);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.to_string().contains("Link not found"));
    }

    #[test]
    fn test_graceful_degradation_on_instance_drop() {
        // Test that data writer/reader gracefully degrade when LinkInstance is dropped
        let output: LinkOutput<VideoFrame> = LinkOutput::new("test_output");
        let input: LinkInput<VideoFrame> = LinkInput::new("test_input");

        let link_id = link_id::__private::new_unchecked("test_link".to_string());

        // Create instance in a scope so it gets dropped
        {
            let instance = LinkInstance::<VideoFrame>::new(link_id.clone(), 16);
            let output_data_writer = instance.create_link_output_data_writer();
            let input_data_reader = instance.create_link_input_data_reader();

            output
                .add_data_writer(link_id.clone(), output_data_writer)
                .unwrap();
            input
                .add_data_reader(link_id.clone(), input_data_reader, None)
                .unwrap();

            assert!(output.is_connected());
            assert!(input.is_connected());
        }
        // LinkInstance dropped here

        // Data writer/reader should gracefully degrade - is_connected returns false
        // The ports will automatically clean up dead connections on next read/write
        assert!(!output.is_connected());
        assert!(!input.is_connected());
    }

    #[test]
    fn test_multiple_processors_pipeline() {
        let mut runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::Manual)
            .build();

        // Create a chain: source -> middle -> sink
        let source = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add source");
        let middle = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add middle");
        let sink = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add sink");

        // Connect the chain
        let link1 = runtime
            .connect(
                format!("{}.output", source.id),
                format!("{}.input", middle.id),
            )
            .expect("connect source->middle");

        let link2 = runtime
            .connect(
                format!("{}.output", middle.id),
                format!("{}.input", sink.id),
            )
            .expect("connect middle->sink");

        // Verify topology in graph (added immediately)
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.processor_count(), 3);
            assert_eq!(graph.link_count(), 2);
        }

        // Disconnect middle link - queues the operation
        runtime.disconnect(&link1).expect("disconnect");

        // Commit before start does nothing - link should still exist
        runtime.commit().expect("commit before start");
        {
            let graph = runtime.graph().read();
            assert_eq!(
                graph.link_count(),
                2,
                "Links should still exist before start"
            );
        }

        // Start runtime - executes pending operations
        runtime.start().expect("start");

        // Verify partial topology after start
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.processor_count(), 3);
            assert_eq!(
                graph.link_count(),
                1,
                "One link should be removed after start"
            );
            assert!(graph.get_link(&link2.id).is_some());
            assert!(graph.get_link(&link1.id).is_none());
        }

        runtime.stop().expect("stop");
    }

    #[test]
    fn test_disconnect_by_id() {
        let mut runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::Manual)
            .build();

        let source = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add source");

        let sink = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add sink");

        let link = runtime
            .connect(
                format!("{}.output", source.id),
                format!("{}.input", sink.id),
            )
            .expect("connect");

        // Verify link exists in graph
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.link_count(), 1);
        }

        // Disconnect by ID - queues the operation
        runtime
            .disconnect_by_id(&link.id)
            .expect("disconnect by id");

        // Commit before start does nothing
        runtime.commit().expect("commit before start");
        {
            let graph = runtime.graph().read();
            assert_eq!(
                graph.link_count(),
                1,
                "Link should still exist before start"
            );
        }

        // Start runtime - executes pending operations
        runtime.start().expect("start");

        // Verify removed after start
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.link_count(), 0, "Link should be removed after start");
        }

        runtime.stop().expect("stop");
    }
}

// Test delta computation
mod delta_tests {
    use std::collections::HashSet;
    use streamlib::core::compiler::compute_delta;
    use streamlib::core::graph::ProcessorId;
    use streamlib::core::links::LinkUniqueId;

    fn link_id(s: &str) -> LinkUniqueId {
        streamlib::core::links::graph::link_id::__private::new_unchecked(s.to_string())
    }

    #[test]
    fn test_empty_delta() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();
        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_processors_to_add() {
        let mut graph_procs: HashSet<ProcessorId> = HashSet::new();
        graph_procs.insert("proc_a".to_string());
        graph_procs.insert("proc_b".to_string());

        let graph_links: HashSet<LinkUniqueId> = HashSet::new();
        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert_eq!(delta.processors_to_add.len(), 2);
        assert!(delta.processors_to_add.contains(&"proc_a".to_string()));
        assert!(delta.processors_to_add.contains(&"proc_b".to_string()));
        assert!(delta.processors_to_remove.is_empty());
    }

    #[test]
    fn test_processors_to_remove() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();

        let mut running_procs: HashSet<ProcessorId> = HashSet::new();
        running_procs.insert("old_proc".to_string());

        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert!(delta.processors_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert!(delta.processors_to_remove.contains(&"old_proc".to_string()));
    }

    #[test]
    fn test_links_to_add() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let mut graph_links: HashSet<LinkUniqueId> = HashSet::new();
        graph_links.insert(link_id("link_1"));

        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert_eq!(delta.links_to_add.len(), 1);
        assert!(delta.links_to_remove.is_empty());
    }

    #[test]
    fn test_links_to_remove() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();

        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let mut wired_links: HashSet<LinkUniqueId> = HashSet::new();
        wired_links.insert(link_id("old_link"));

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert!(delta.links_to_add.is_empty());
        assert_eq!(delta.links_to_remove.len(), 1);
    }

    #[test]
    fn test_mixed_delta() {
        // Graph has proc_a, proc_b (desired)
        let mut graph_procs: HashSet<ProcessorId> = HashSet::new();
        graph_procs.insert("proc_a".to_string());
        graph_procs.insert("proc_b".to_string());

        // Graph has link_1 (desired)
        let mut graph_links: HashSet<LinkUniqueId> = HashSet::new();
        graph_links.insert(link_id("link_1"));

        // Running has proc_a, proc_c (current)
        let mut running_procs: HashSet<ProcessorId> = HashSet::new();
        running_procs.insert("proc_a".to_string());
        running_procs.insert("proc_c".to_string());

        // Wired has link_2 (current)
        let mut wired_links: HashSet<LinkUniqueId> = HashSet::new();
        wired_links.insert(link_id("link_2"));

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        // Should add proc_b (in graph, not running)
        assert_eq!(delta.processors_to_add.len(), 1);
        assert!(delta.processors_to_add.contains(&"proc_b".to_string()));

        // Should remove proc_c (running, not in graph)
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert!(delta.processors_to_remove.contains(&"proc_c".to_string()));

        // Should add link_1 (in graph, not wired)
        assert_eq!(delta.links_to_add.len(), 1);

        // Should remove link_2 (wired, not in graph)
        assert_eq!(delta.links_to_remove.len(), 1);
    }
}

// Test config hot-reload
mod config_tests {
    use streamlib::core::graph::compute_config_checksum;
    use streamlib::core::processors::{SimplePassthroughConfig, SimplePassthroughProcessor};
    use streamlib::core::runtime::StreamRuntime;

    #[test]
    fn test_config_checksum_computed_on_add() {
        let mut runtime = StreamRuntime::new();

        let node = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add processor");

        // Checksum should be computed for the default config
        let graph = runtime.graph().read();
        let processor = graph.get_processor(&node).expect("get processor");

        // Default config should have a non-zero checksum
        assert!(processor.config_checksum != 0);
    }

    #[test]
    fn test_config_checksum_unchanged_for_same_config() {
        let mut runtime = StreamRuntime::new();

        let node = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add processor");

        // Get original checksum
        let original_checksum = {
            let graph = runtime.graph().read();
            graph
                .get_processor(&node)
                .expect("get processor")
                .config_checksum
        };

        // Update with same config
        runtime
            .update_processor_config(&node, SimplePassthroughConfig::default())
            .expect("update config");

        // Checksum should be the same for identical config
        let new_checksum = {
            let graph = runtime.graph().read();
            graph
                .get_processor(&node)
                .expect("get processor")
                .config_checksum
        };

        assert_eq!(original_checksum, new_checksum);
    }

    #[test]
    fn test_config_checksum_changes_for_different_config() {
        let mut runtime = StreamRuntime::new();

        let node = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add processor");

        // Get original checksum
        let original_checksum = {
            let graph = runtime.graph().read();
            graph
                .get_processor(&node)
                .expect("get processor")
                .config_checksum
        };

        // Update with different config (scale = 2.0 instead of default 1.0)
        runtime
            .update_processor_config(&node, SimplePassthroughConfig { scale: 2.0 })
            .expect("update config");

        // Checksum should be different for different config
        let new_checksum = {
            let graph = runtime.graph().read();
            graph
                .get_processor(&node)
                .expect("get processor")
                .config_checksum
        };

        assert_ne!(original_checksum, new_checksum);
    }

    #[test]
    fn test_compute_config_checksum_deterministic() {
        #[derive(Debug)]
        struct TestConfig {
            value: i32,
            name: String,
        }

        let config1 = TestConfig {
            value: 42,
            name: "test".to_string(),
        };
        let config2 = TestConfig {
            value: 42,
            name: "test".to_string(),
        };
        let config3 = TestConfig {
            value: 99,
            name: "different".to_string(),
        };

        // Same config = same checksum
        assert_eq!(
            compute_config_checksum(&config1),
            compute_config_checksum(&config2)
        );

        // Different config = different checksum
        assert_ne!(
            compute_config_checksum(&config1),
            compute_config_checksum(&config3)
        );
    }

    #[test]
    fn test_graph_update_processor_config() {
        use streamlib::core::graph::Graph;

        let mut graph = Graph::new();

        // Add processor manually
        let node = graph
            .add_processor_node::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add processor");

        let original_checksum = graph
            .get_processor(&node)
            .expect("get processor")
            .config_checksum;

        // Update with a different JSON config
        let new_config = serde_json::json!({"scale": 5.0});
        graph
            .update_processor_config(&node, new_config.clone())
            .expect("update config");

        // New checksum should be different
        let updated_checksum = graph
            .get_processor(&node)
            .expect("get processor")
            .config_checksum;

        assert_ne!(original_checksum, updated_checksum);

        // Config should be updated
        let processor = graph.get_processor(&node).expect("get processor");
        assert_eq!(processor.config, Some(new_config));
    }
}

/// Tests that verify macro-generated update_config() works correctly
mod macro_update_config_tests {
    use streamlib::core::processors::SimplePassthroughConfig;
    use streamlib::core::Processor;

    /// Test that macro generates update_config() that actually updates the #[config] field
    #[test]
    fn test_macro_generates_update_config() {
        use streamlib::core::processors::SimplePassthroughProcessor;

        // Create processor with default config
        let mut processor = SimplePassthroughProcessor::Processor::from_config(Default::default())
            .expect("create processor");

        // Verify initial config
        assert_eq!(processor.scale(), 1.0);

        // Create new config
        let new_config = SimplePassthroughConfig { scale: 5.0 };

        // Call update_config - should update the internal #[config] field
        processor
            .update_config(new_config)
            .expect("update_config should succeed");

        // Verify config was updated
        assert_eq!(processor.scale(), 5.0);
    }

    /// Test that apply_config_json works with macro-generated processor
    #[test]
    fn test_macro_apply_config_json() {
        use streamlib::core::processors::SimplePassthroughProcessor;

        // Create processor with default config
        let mut processor = SimplePassthroughProcessor::Processor::from_config(Default::default())
            .expect("create processor");

        // Verify initial config
        assert_eq!(processor.scale(), 1.0);

        // Apply JSON config
        let config_json = serde_json::json!({"scale": 3.14});
        processor
            .apply_config_json(&config_json)
            .expect("apply_config_json should succeed");

        // Verify config was updated
        assert!((processor.scale() - 3.14).abs() < 0.001);
    }

    /// Test that update_config can be called multiple times
    #[test]
    fn test_macro_update_config_multiple_times() {
        use streamlib::core::processors::SimplePassthroughProcessor;

        let mut processor = SimplePassthroughProcessor::Processor::from_config(Default::default())
            .expect("create processor");

        // Update multiple times
        for i in 1..=5 {
            let config = SimplePassthroughConfig {
                scale: i as f32 * 2.0,
            };
            processor
                .update_config(config)
                .expect("update_config should succeed");
            assert_eq!(processor.scale(), i as f32 * 2.0);
        }
    }
}
