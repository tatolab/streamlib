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
    let runtime = StreamRuntime::new();
    assert_eq!(runtime.commit_mode(), CommitMode::Auto);
}

#[test]
fn test_runtime_manual_commit_mode() {
    let runtime = StreamRuntime::with_commit_mode(CommitMode::Manual);
    assert_eq!(runtime.commit_mode(), CommitMode::Manual);
}

#[test]
fn test_auto_commit_syncs_graph_changes() {
    let mut runtime = StreamRuntime::new();
    assert_eq!(runtime.commit_mode(), CommitMode::Auto);

    // Add a processor - should auto-commit
    let node = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add processor");

    // Verify node was added to graph
    let graph = runtime.graph().read();
    assert!(graph.has_processor(&node.id));
    assert_eq!(graph.processor_count(), 1);
}

#[test]
fn test_manual_commit_batches_changes() {
    let mut runtime = StreamRuntime::with_commit_mode(CommitMode::Manual);

    // Add multiple processors without committing
    let node1 = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add processor 1");

    let node2 = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
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
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add source");

    let sink = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
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
    let mut runtime = StreamRuntime::new();

    // Setup: add processors and connect
    let source = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add source");

    let sink = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add sink");

    let link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("Failed to connect");

    // Verify link exists
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.link_count(), 1);
    }

    // Disconnect
    runtime.disconnect(&link).expect("Failed to disconnect");

    // Verify link is removed
    {
        let graph = runtime.graph().read();
        assert_eq!(graph.link_count(), 0);
    }
}

#[test]
fn test_remove_processor() {
    let mut runtime = StreamRuntime::new();

    let node = runtime
        .add_processor::<SimplePassthroughProcessor>(Default::default())
        .expect("Failed to add processor");

    // Verify exists
    {
        let graph = runtime.graph().read();
        assert!(graph.has_processor(&node.id));
    }

    // Remove
    runtime.remove_processor(&node).expect("Failed to remove processor");

    // Verify removed
    {
        let graph = runtime.graph().read();
        assert!(!graph.has_processor(&node.id));
        assert_eq!(graph.processor_count(), 0);
    }
}

#[test]
fn test_set_commit_mode() {
    let mut runtime = StreamRuntime::new();
    assert_eq!(runtime.commit_mode(), CommitMode::Auto);

    runtime.set_commit_mode(CommitMode::Manual);
    assert_eq!(runtime.commit_mode(), CommitMode::Manual);

    runtime.set_commit_mode(CommitMode::Auto);
    assert_eq!(runtime.commit_mode(), CommitMode::Auto);
}

// Test the factory registration and lookup
#[test]
fn test_registry_factory_registration() {
    let factory = RegistryBackedFactory::new();

    // Before registration
    assert!(!factory.is_registered("SimplePassthroughProcessor"));
    assert!(!factory.can_create("SimplePassthroughProcessor"));

    // Register
    factory.register::<SimplePassthroughProcessor>();

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

// Test delta computation
mod delta_tests {
    use std::collections::HashSet;
    use streamlib::core::executor::compute_delta;
    use streamlib::core::graph::ProcessorId;
    use streamlib::core::link_channel::LinkId;

    fn link_id(s: &str) -> LinkId {
        streamlib::core::link_channel::link_id::__private::new_unchecked(s.to_string())
    }

    #[test]
    fn test_empty_delta() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let graph_links: HashSet<LinkId> = HashSet::new();
        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_processors_to_add() {
        let mut graph_procs: HashSet<ProcessorId> = HashSet::new();
        graph_procs.insert("proc_a".to_string());
        graph_procs.insert("proc_b".to_string());

        let graph_links: HashSet<LinkId> = HashSet::new();
        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkId> = HashSet::new();

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
        let graph_links: HashSet<LinkId> = HashSet::new();

        let mut running_procs: HashSet<ProcessorId> = HashSet::new();
        running_procs.insert("old_proc".to_string());

        let wired_links: HashSet<LinkId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert!(delta.processors_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert!(delta.processors_to_remove.contains(&"old_proc".to_string()));
    }

    #[test]
    fn test_links_to_add() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let mut graph_links: HashSet<LinkId> = HashSet::new();
        graph_links.insert(link_id("link_1"));

        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let wired_links: HashSet<LinkId> = HashSet::new();

        let delta = compute_delta(&graph_procs, &graph_links, &running_procs, &wired_links);

        assert!(!delta.is_empty());
        assert_eq!(delta.links_to_add.len(), 1);
        assert!(delta.links_to_remove.is_empty());
    }

    #[test]
    fn test_links_to_remove() {
        let graph_procs: HashSet<ProcessorId> = HashSet::new();
        let graph_links: HashSet<LinkId> = HashSet::new();

        let running_procs: HashSet<ProcessorId> = HashSet::new();
        let mut wired_links: HashSet<LinkId> = HashSet::new();
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
        let mut graph_links: HashSet<LinkId> = HashSet::new();
        graph_links.insert(link_id("link_1"));

        // Running has proc_a, proc_c (current)
        let mut running_procs: HashSet<ProcessorId> = HashSet::new();
        running_procs.insert("proc_a".to_string());
        running_procs.insert("proc_c".to_string());

        // Wired has link_2 (current)
        let mut wired_links: HashSet<LinkId> = HashSet::new();
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
