// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Basic Query Operations.

use super::*;

#[test]
fn test_v_all_returns_all_processors() {
    let mut graph = test_graph();

    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    let ids = graph.traversal().v(()).ids();
    assert_eq!(ids.len(), 3);
}

#[test]
fn test_v_by_id_returns_specific_processor() {
    let mut graph = test_graph();

    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    let found = graph.traversal().v(id.as_str()).first();
    assert!(found.is_some());
    assert_eq!(
        found.unwrap().processor_type,
        MockProcessor::Processor::processor_class_import_path()
    );
}

#[test]
fn test_v_nonexistent_returns_empty() {
    let graph = test_graph();

    let found = graph.traversal().v("nonexistent").first();
    assert!(found.is_none());
}

#[test]
fn test_exists_true_when_found() {
    let mut graph = test_graph();
    let id = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    assert!(graph.traversal().v(id.as_str()).exists());
}

#[test]
fn test_exists_false_when_not_found() {
    let graph = test_graph();

    assert!(!graph.traversal().v("nonexistent").exists());
}

#[test]
fn test_ids_returns_all_processor_ids() {
    let mut graph = test_graph();

    let id1 = graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();
    let id2 = graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named")
        .first()
        .expect("should create processor")
        .id
        .to_string();

    let ids = graph.traversal().v(()).ids();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().any(|id| id.as_str() == id1));
    assert!(ids.iter().any(|id| id.as_str() == id2));
}

#[test]
fn test_first_returns_some_processor() {
    let mut graph = test_graph();
    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    let first = graph.traversal().v(()).first();
    assert!(first.is_some());
}

#[test]
fn test_first_on_empty_returns_none() {
    let graph = test_graph();

    assert!(graph.traversal().v(()).first().is_none());
}

#[test]
fn test_iter_yields_all_processors() {
    let mut graph = test_graph();

    graph
        .traversal_mut()
        .add_v(MockProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");
    graph
        .traversal_mut()
        .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
        .expect("the node is named");

    let class_paths: Vec<_> = graph
        .traversal()
        .v(())
        .iter()
        .map(|n| n.processor_type.clone())
        .collect();

    assert_eq!(class_paths.len(), 3);
    for expected in [
        MockProcessor::Processor::processor_class_import_path(),
        MockOutputOnlyProcessor::Processor::processor_class_import_path(),
        MockInputOnlyProcessor::Processor::processor_class_import_path(),
    ] {
        assert!(class_paths.contains(&expected), "missing {expected}");
    }
}
