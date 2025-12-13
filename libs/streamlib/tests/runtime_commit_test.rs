// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Integration tests for the commit-based runtime synchronization flow.
///
/// Tests the Auto/Manual commit modes and delta-based synchronization between
/// Graph (desired state) and ExecutionGraph (running state).
use streamlib::core::processors::SimplePassthroughProcessor;
use streamlib::core::runtime::{CommitMode, StreamRuntime};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};

#[test]
fn test_runtime_default_is_auto_commit() {
    // Default runtime uses auto-commit - verify by checking behavior
    let mut runtime = StreamRuntime::new();

    // Add a processor - should be added to graph immediately (auto-commit behavior)
    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    let graph = runtime.graph().read();
    assert!(graph.traversal().v(&node).exists());
}

#[test]
fn test_runtime_manual_commit_mode() {
    // Manual mode requires explicit commit - verify by checking behavior
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::BatchManually)
        .build();

    // Add processor - added to graph but pending operations queued
    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    // Processor exists in graph (topology added immediately)
    let graph = runtime.graph().read();
    assert!(graph.traversal().v(&node).exists());
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
    assert!(graph.traversal().v(&node).exists());
    assert_eq!(graph.traversal().v(()).iter().count(), 1);
}

#[test]
fn test_manual_commit_batches_changes() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::BatchManually)
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
        assert!(graph.traversal().v(&node1).exists());
        assert!(graph.traversal().v(&node2).exists());
        assert_eq!(graph.traversal().v(()).iter().count(), 2);
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
            OutputLinkPortRef::new(source.clone(), "output"),
            InputLinkPortRef::new(sink.clone(), "input"),
        )
        .expect("Failed to connect");

    // Verify link was added to graph
    let graph = runtime.graph().read();
    assert_eq!(graph.traversal().e(()).iter().count(), 1);
    assert!(graph.traversal().e(&link).exists());
}

#[test]
fn test_disconnect_removes_link() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::BatchManually)
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
            OutputLinkPortRef::new(source.clone(), "output"),
            InputLinkPortRef::new(sink.clone(), "input"),
        )
        .expect("Failed to connect");

    // Verify link exists in graph (added immediately to graph structure)
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.traversal().e(()).iter().count(), 1);
    }

    // Disconnect - queues the operation
    runtime
        .disconnect_by_id(&link)
        .expect("Failed to disconnect");

    // Commit before start does nothing - link should still exist
    runtime.commit().expect("Commit before start");
    {
        let graph = runtime.graph().read();
        assert_eq!(
            graph.traversal().e(()).iter().count(),
            1,
            "Link should still exist before start"
        );
    }

    // Start runtime - executes pending operations (including the disconnect)
    runtime.start().expect("Failed to start");

    // Verify link is removed after start
    {
        let graph = runtime.graph().read();
        assert_eq!(
            graph.traversal().e(()).iter().count(),
            0,
            "Link should be removed after start"
        );
    }

    runtime.stop().expect("Failed to stop");
}

#[test]
fn test_remove_processor() {
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::BatchManually)
        .build();

    let node = runtime
        .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
        .expect("Failed to add processor");

    // Verify exists in graph (added immediately to graph structure)
    {
        let graph = runtime.graph().read();
        assert!(graph.traversal().v(&node).exists());
    }

    // Remove - queues the operation
    runtime
        .remove_processor_by_id(&node)
        .expect("Failed to remove processor");

    // Commit before start does nothing - processor should still exist
    runtime.commit().expect("Commit before start");
    {
        let graph = runtime.graph().read();
        assert!(
            graph.traversal().v(&node).exists(),
            "Processor should still exist before start"
        );
        assert_eq!(graph.traversal().v(()).iter().count(), 1);
    }

    // Start runtime - executes pending operations (including the remove)
    runtime.start().expect("Failed to start");

    // Verify removed after start
    {
        let graph = runtime.graph().read();
        assert!(
            !graph.traversal().v(&node).exists(),
            "Processor should be removed after start"
        );
        assert_eq!(graph.traversal().v(()).iter().count(), 0);
    }

    runtime.stop().expect("Failed to stop");
}

// Test delta computation
mod delta_tests {
    use std::collections::HashSet;
    use streamlib::core::compiler::compute_delta;
    use streamlib::core::{LinkUniqueId, ProcessorUniqueId};

    #[test]
    fn test_empty_delta() {
        let graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();
        let running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_processors_to_add() {
        let mut graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        graph_procs.insert("proc_a".into());
        graph_procs.insert("proc_b".into());

        let graph_links: HashSet<LinkUniqueId> = HashSet::new();
        let running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert_eq!(delta.processors_to_add.len(), 2);
        assert!(delta.processors_to_add.contains(&"proc_a".into()));
        assert!(delta.processors_to_add.contains(&"proc_b".into()));
        assert!(delta.processors_to_remove.is_empty());
    }

    #[test]
    fn test_processors_to_remove() {
        let graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();

        let mut running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        running_procs.insert("old_proc".into());

        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert!(delta.processors_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert!(delta.processors_to_remove.contains(&"old_proc".into()));
    }

    #[test]
    fn test_links_to_add() {
        let graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let mut graph_links: HashSet<LinkUniqueId> = HashSet::new();
        graph_links.insert("link_1".into());

        let running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let wired_links: HashSet<LinkUniqueId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert_eq!(delta.links_to_add.len(), 1);
        assert!(delta.links_to_remove.is_empty());
    }

    #[test]
    fn test_links_to_remove() {
        let graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let graph_links: HashSet<LinkUniqueId> = HashSet::new();

        let running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        let mut wired_links: HashSet<LinkUniqueId> = HashSet::new();
        wired_links.insert("old_link".into());

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert!(delta.links_to_add.is_empty());
        assert_eq!(delta.links_to_remove.len(), 1);
    }

    #[test]
    fn test_mixed_delta() {
        // Graph has proc_a, proc_b (desired)
        let mut graph_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        graph_procs.insert("proc_a".into());
        graph_procs.insert("proc_b".into());

        // Graph has link_1 (desired)
        let mut graph_links: HashSet<LinkUniqueId> = HashSet::new();
        graph_links.insert("link_1".into());

        // Running has proc_a, proc_c (current)
        let mut running_procs: HashSet<ProcessorUniqueId> = HashSet::new();
        running_procs.insert("proc_a".into());
        running_procs.insert("proc_c".into());

        // Wired has link_2 (current)
        let mut wired_links: HashSet<LinkUniqueId> = HashSet::new();
        wired_links.insert("link_2".into());

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        // Should add proc_b (in graph, not running)
        assert_eq!(delta.processors_to_add.len(), 1);
        assert!(delta.processors_to_add.contains(&"proc_b".into()));

        // Should remove proc_c (running, not in graph)
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert!(delta.processors_to_remove.contains(&"proc_c".into()));

        // Should add link_1 (in graph, not wired)
        assert_eq!(delta.links_to_add.len(), 1);

        // Should remove link_2 (wired, not in graph)
        assert_eq!(delta.links_to_remove.len(), 1);
    }
}

// Test config update at runtime
mod config_tests {
    use streamlib::core::processors::{SimplePassthroughConfig, SimplePassthroughProcessor};
    use streamlib::core::runtime::StreamRuntime;

    #[test]
    fn test_config_update_via_runtime() {
        let mut runtime = StreamRuntime::new();

        let node = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add processor");

        // Update with different config
        runtime
            .update_processor_config(&node, SimplePassthroughConfig { scale: 2.0 })
            .expect("update config");

        // Verify config was stored in graph
        let graph = runtime.graph().read();
        let processor = graph.traversal().v(&node).first().expect("get processor");
        assert!(processor.config.is_some());
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
        let config_json = serde_json::json!({"scale": 3.5});
        processor
            .apply_config_json(&config_json)
            .expect("apply_config_json should succeed");

        // Verify config was updated
        assert!((processor.scale() - 3.5).abs() < 0.001);
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

// Test pipeline with multiple processors
mod pipeline_tests {
    use streamlib::core::processors::SimplePassthroughProcessor;
    use streamlib::core::runtime::{CommitMode, StreamRuntime};
    use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_multiple_processors_pipeline() {
        let mut runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::BatchManually)
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
                OutputLinkPortRef::new(source.clone(), "output"),
                InputLinkPortRef::new(middle.clone(), "input"),
            )
            .expect("connect source->middle");

        let link2 = runtime
            .connect(
                OutputLinkPortRef::new(middle.clone(), "output"),
                InputLinkPortRef::new(sink.clone(), "input"),
            )
            .expect("connect middle->sink");

        // Verify topology in graph (added immediately)
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.traversal().v(()).iter().count(), 3);
            assert_eq!(graph.traversal().e(()).iter().count(), 2);
        }

        // Disconnect middle link - queues the operation
        runtime.disconnect_by_id(&link1).expect("disconnect");

        // Commit before start does nothing - link should still exist
        runtime.commit().expect("commit before start");
        {
            let graph = runtime.graph().read();
            assert_eq!(
                graph.traversal().e(()).iter().count(),
                2,
                "Links should still exist before start"
            );
        }

        // Start runtime - executes pending operations
        runtime.start().expect("start");

        // Verify partial topology after start
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.traversal().v(()).iter().count(), 3);
            assert_eq!(
                graph.traversal().e(()).iter().count(),
                1,
                "One link should be removed after start"
            );
            assert!(graph.traversal().e(&link2).exists());
            assert!(!graph.traversal().e(&link1).exists());
        }

        runtime.stop().expect("stop");
    }

    #[test]
    fn test_disconnect_by_id() {
        let mut runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::BatchManually)
            .build();

        let source = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add source");

        let sink = runtime
            .add_processor::<SimplePassthroughProcessor::Processor>(Default::default())
            .expect("add sink");

        let link = runtime
            .connect(
                OutputLinkPortRef::new(source.clone(), "output"),
                InputLinkPortRef::new(sink.clone(), "input"),
            )
            .expect("connect");

        // Verify link exists in graph
        {
            let graph = runtime.graph().read();
            assert_eq!(graph.traversal().e(()).iter().count(), 1);
        }

        // Disconnect by ID - queues the operation
        runtime.disconnect_by_id(&link).expect("disconnect by id");

        // Commit before start does nothing
        runtime.commit().expect("commit before start");
        {
            let graph = runtime.graph().read();
            assert_eq!(
                graph.traversal().e(()).iter().count(),
                1,
                "Link should still exist before start"
            );
        }

        // Start runtime - executes pending operations
        runtime.start().expect("start");

        // Verify removed after start
        {
            let graph = runtime.graph().read();
            assert_eq!(
                graph.traversal().e(()).iter().count(),
                0,
                "Link should be removed after start"
            );
        }

        runtime.stop().expect("stop");
    }
}
