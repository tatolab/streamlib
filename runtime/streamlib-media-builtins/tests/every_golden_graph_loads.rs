// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The compatibility record: every golden graph in `golden_graphs/` loads on
//! this build. A golden is never edited — a change to the graph's shape, a
//! built-in or a setting adds one beside it — so a graph an older stream
//! recorded stays provably loadable by every later runtime.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::Runner;
use streamlib_media_builtins::register_media_builtin_processor_types;

fn golden_graph_paths() -> Vec<PathBuf> {
    let golden_graphs_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden_graphs");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&golden_graphs_directory)
        .expect("the golden graphs are checked in beside this test")
        .map(|entry| entry.expect("a directory entry reads").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no golden graph under {golden_graphs_directory:?}"
    );
    paths
}

fn golden_graph_document(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("a golden graph reads"))
        .expect("a golden graph is JSON")
}

fn node_types_in(graph_document: &serde_json::Value) -> Vec<ProcessorClassImportPath> {
    graph_document["nodes"]
        .as_array()
        .expect("a graph holds nodes")
        .iter()
        .map(|node| {
            ProcessorClassImportPath::new(node["type"].as_str().expect("a node has a type"))
                .expect("a type names a class")
        })
        .collect()
}

/// `graph_document` without the nodes named in `node_names`, and every link
/// and exposure that names one of them.
fn without_the_nodes(
    graph_document: &serde_json::Value,
    node_names: &BTreeSet<String>,
) -> serde_json::Value {
    let is_removed = |node_name: &serde_json::Value| {
        node_name
            .as_str()
            .is_some_and(|node_name| node_names.contains(node_name))
    };
    let mut remaining = graph_document.clone();
    let mut retain_in = |collection: &str, keep: &dyn Fn(&serde_json::Value) -> bool| {
        if let Some(objects) = remaining[collection].as_array_mut() {
            objects.retain(keep);
        }
    };
    retain_in("nodes", &|node| !is_removed(&node["name"]));
    retain_in("links", &|link| {
        !is_removed(&link["source"]["node"]) && !is_removed(&link["target"]["node"])
    });
    retain_in("exposed", &|exposure| !is_removed(&exposure["node"]));
    remaining
}

/// A golden naming a built-in this floor compiles out is refused for it,
/// naming the floor, and loads whole once those nodes are taken out — which is
/// as much of it as this floor can run.
#[test]
fn every_golden_graph_loads_on_this_floor() {
    register_media_builtin_processor_types();

    for path in golden_graph_paths() {
        let graph_document = golden_graph_document(&path);
        let graph = GraphSnapshot::from_graph_document(graph_document.clone())
            .unwrap_or_else(|refusal| panic!("{path:?} no longer reads: {refusal}"));
        let node_names_compiled_out_here: BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|node| {
                PROCESSOR_REGISTRY
                    .refuse_a_built_in_node_type_absent_on_this_floor(&node.processor_type)
                    .is_err()
            })
            .map(|node| node.name.clone())
            .collect();

        let loaded = Runner::new()
            .expect("a runtime constructs")
            .load_graph_snapshot(&graph);

        if node_names_compiled_out_here.is_empty() {
            loaded.unwrap_or_else(|refusal| panic!("{path:?} no longer loads: {refusal}"));
            continue;
        }
        match loaded {
            Err(Error::BuiltInNodeTypeAbsentOnThisFloor { .. }) => {}
            other => panic!(
                "{path:?} names {node_names_compiled_out_here:?}, which this floor compiles out, \
                 and should be refused naming the floor: got {other:?}"
            ),
        }
        let the_rest = GraphSnapshot::from_graph_document(without_the_nodes(
            &graph_document,
            &node_names_compiled_out_here,
        ))
        .expect("a golden without some of its nodes still reads");
        Runner::new()
            .expect("a runtime constructs")
            .load_graph_snapshot(&the_rest)
            .unwrap_or_else(|refusal| {
                panic!(
                    "{path:?} without {node_names_compiled_out_here:?} no longer loads: {refusal}"
                )
            });
    }
}

/// A built-in no golden names is one whose graphs nothing yet holds to load
/// on later builds; adding one adds a golden that names it.
#[test]
fn the_golden_graphs_name_every_built_in_this_floor_registers() {
    register_media_builtin_processor_types();
    let named_by_a_golden: BTreeSet<ProcessorClassImportPath> = golden_graph_paths()
        .iter()
        .flat_map(|path| node_types_in(&golden_graph_document(path)))
        .collect();

    let named_by_no_golden: Vec<ProcessorClassImportPath> = PROCESSOR_REGISTRY
        .registered_processor_class_import_paths()
        .into_iter()
        .filter(|registered| registered.names_a_built_in_node())
        .filter(|built_in| !named_by_a_golden.contains(built_in))
        .collect();

    assert!(
        named_by_no_golden.is_empty(),
        "no golden graph names {named_by_no_golden:?} — add a golden beside the others that does"
    );
}
