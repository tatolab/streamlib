// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The compatibility record: every golden graph in `golden_graphs/` loads on
//! this build. A golden is never edited — a change to the graph's shape, a
//! built-in or a setting adds one beside it — so a graph an older stream
//! recorded stays provably loadable by every later runtime. A golden is
//! deleted only by the breaking change that retires a shape it holds, which
//! adds the golden replacing it beside the others.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{OptionsForLoadingOneStream, Runner};
use streamlib_media_builtins::register_media_builtin_processor_types;

/// Each golden graph's SHA-256, pinned as it was added: a golden is never
/// edited, so a change to what a graph holds adds a golden and a line here,
/// and only the breaking change retiring a shape a golden holds drops its line.
const GOLDEN_GRAPH_SHA256_BY_FILE_NAME: &[(&str, &str)] = &[
    (
        "0011-every-key-every-built-in-and-the-audio-and-opus-modules.json",
        "11f3985b4b8c1fc288ccb7d2387759e1c243e98550f9ddd9da24cadd64b3fc0f",
    ),
    (
        "0012-every-key-with-exposure-levels-and-the-audio-and-opus-modules.json",
        "f427ca66e328f8023573a79de14cb921bbed976ca661c402e07a039f041599bb",
    ),
    (
        "0013-every-key-with-a-relay-track-source-by-broadcast-and-track-and-the-audio-and-opus-modules.json",
        "a562cb735c0beb7a1144d250ce57c3c789a062e2dfedd67c0a855830c035e330",
    ),
    (
        "0014-every-key-with-a-relay-track-source-reading-with-a-max-age-and-the-audio-and-opus-modules.json",
        "342e9b72ce48c8106a0fc373339338e8a3b880db1132104b93626ce4d4fad31c",
    ),
];

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

/// Load `graph` as the one stream of a fresh engine, in a project directory of
/// its own.
fn load_on_a_fresh_engine(graph: &GraphSnapshot) -> streamlib::sdk::error::Result<()> {
    let project_directory = tempfile::tempdir().expect("a project directory");
    Runner::new()
        .expect("a runtime constructs")
        .load_stream_from_graph_snapshot(
            graph,
            OptionsForLoadingOneStream::in_project_directory(project_directory.path()),
        )
        .map(drop)
}

/// The names of the nodes in `graph_document` whose type a module declares:
/// each runs in its own helper process from an installed distribution, which a
/// runtime with no interpreter cannot describe.
fn module_node_names_in(graph_document: &serde_json::Value) -> BTreeSet<String> {
    graph_document["nodes"]
        .as_array()
        .expect("a graph holds nodes")
        .iter()
        .filter(|node| {
            !ProcessorClassImportPath::new(node["type"].as_str().expect("a node has a type"))
                .expect("a type names a class")
                .names_a_built_in_node()
        })
        .map(|node| {
            node["name"]
                .as_str()
                .expect("a node has a name")
                .to_string()
        })
        .collect()
}

/// A golden reads whole; with its module nodes taken out it loads on a runtime
/// with no interpreter. A built-in this floor compiles out is refused for it,
/// naming the floor, and the rest loads once those nodes are taken out too —
/// which is as much of it as this floor can run.
#[test]
fn every_golden_graph_loads_on_this_floor() {
    register_media_builtin_processor_types();

    for path in golden_graph_paths() {
        let recorded_graph_document = golden_graph_document(&path);
        GraphSnapshot::from_graph_document(recorded_graph_document.clone())
            .unwrap_or_else(|refusal| panic!("{path:?} no longer reads: {refusal}"));
        let graph_document = without_the_nodes(
            &recorded_graph_document,
            &module_node_names_in(&recorded_graph_document),
        );
        let graph =
            GraphSnapshot::from_graph_document(graph_document.clone()).unwrap_or_else(|refusal| {
                panic!("{path:?} without its module nodes no longer reads: {refusal}")
            });
        let node_names_compiled_out_here: BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|node| {
                PROCESSOR_REGISTRY
                    .refuse_a_built_in_node_type_this_runtime_lacks(&node.processor_type)
                    .is_err()
            })
            .map(|node| node.name.clone())
            .collect();

        let loaded = load_on_a_fresh_engine(&graph);

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
        load_on_a_fresh_engine(&the_rest).unwrap_or_else(|refusal| {
            panic!("{path:?} without {node_names_compiled_out_here:?} no longer loads: {refusal}")
        });
    }
}

/// A golden names each module that replaced a built-in by the type its
/// distribution declares, never by the built-in type it replaced.
#[test]
fn the_golden_graphs_name_the_moved_built_ins_by_their_module_types() {
    register_media_builtin_processor_types();
    for path in golden_graph_paths() {
        let node_types = node_types_in(&golden_graph_document(&path));
        for module_node_type in [
            "tatolab.microphone:MicrophoneSource",
            "tatolab.speaker:SpeakerSink",
            "tatolab.opus:OpusEncoder",
            "tatolab.opus:OpusDecoder",
        ] {
            assert!(
                node_types
                    .iter()
                    .any(|node_type| node_type.as_str() == module_node_type),
                "{path:?} does not name {module_node_type}"
            );
        }
        for node_type in &node_types {
            assert!(
                !matches!(
                    PROCESSOR_REGISTRY.refuse_a_built_in_node_type_this_runtime_lacks(node_type),
                    Err(Error::BuiltInNodeTypeMovedToAModule { .. })
                ),
                "{path:?} names {node_type}, which moved to a module"
            );
        }
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

/// Making a red golden test green by editing the golden is the failure the
/// record exists to prevent, so an edit is red here whatever else passes.
#[test]
fn no_golden_graph_has_changed_or_gone_since_it_was_pinned() {
    let mut file_names_checked_in = BTreeSet::new();
    for path in golden_graph_paths() {
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("a golden graph's file name is UTF-8")
            .to_string();
        let Some((_, pinned_sha256)) = GOLDEN_GRAPH_SHA256_BY_FILE_NAME
            .iter()
            .find(|(pinned_file_name, _)| *pinned_file_name == file_name)
        else {
            panic!("{file_name} is pinned by no line of GOLDEN_GRAPH_SHA256_BY_FILE_NAME — pin it");
        };
        let sha256: String = Sha256::digest(std::fs::read(&path).expect("a golden graph reads"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();

        assert_eq!(
            sha256, *pinned_sha256,
            "{file_name} has changed since it was pinned. A golden is never edited: restore it \
             and add the new graph as a golden beside it"
        );
        file_names_checked_in.insert(file_name);
    }
    for (pinned_file_name, _) in GOLDEN_GRAPH_SHA256_BY_FILE_NAME {
        assert!(
            file_names_checked_in.contains(*pinned_file_name),
            "{pinned_file_name} is pinned and no longer checked in — a golden is deleted only \
             with its pin, by the breaking change that retires a shape it holds"
        );
    }
}
