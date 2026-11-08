// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The compatibility record of graph documents: every golden document in
//! `golden_graph_documents/` reads on this build, holds a UUID for its stream
//! asset and each node, is written in the canonical form this build writes, and
//! loads once its module nodes are taken out by UUID. A golden is never edited;
//! a change to the document's shape adds one beside it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::runtime::{OptionsForLoadingOneStream, Runner};
use streamlib_media_builtins::register_media_builtin_processor_types;

/// Each golden graph document's SHA-256, pinned as it was added.
const GOLDEN_GRAPH_DOCUMENT_SHA256_BY_FILE_NAME: &[(&str, &str)] = &[(
    "0001-every-media-module-with-node-uuids-links-by-uuid-and-studio-fields.graph.json",
    "65d56576a429a4951b289a6b04685b0e7248f68660edca460bdb74991b032f9c",
)];

fn golden_graph_document_paths() -> Vec<PathBuf> {
    let golden_graph_documents_directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden_graph_documents");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&golden_graph_documents_directory)
        .expect("the golden graph documents are checked in beside this test")
        .map(|entry| entry.expect("a directory entry reads").path())
        .filter(|path| {
            path.file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| file_name.ends_with(".graph.json"))
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no golden graph document under {golden_graph_documents_directory:?}"
    );
    paths
}

fn golden_graph_document(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("a golden document reads"))
        .expect("a golden document is JSON")
}

/// `graph_document` without the nodes whose UUID is in `node_uuids`, and every link whose
/// end names one of them.
fn without_the_nodes_of_the_uuids(
    graph_document: &serde_json::Value,
    node_uuids: &BTreeSet<String>,
) -> serde_json::Value {
    let is_removed = |node_uuid: &serde_json::Value| {
        node_uuid
            .as_str()
            .is_some_and(|node_uuid| node_uuids.contains(node_uuid))
    };
    let mut remaining = graph_document.clone();
    if let Some(nodes) = remaining["nodes"].as_array_mut() {
        nodes.retain(|node| !is_removed(&node["id"]));
    }
    if let Some(links) = remaining["links"].as_array_mut() {
        links.retain(|link| {
            !is_removed(&link["source"]["node"]) && !is_removed(&link["target"]["node"])
        });
    }
    remaining
}

/// The UUIDs of the nodes in `graph_document` whose type a module declares, which a
/// runtime with no interpreter cannot describe.
fn module_node_uuids_in(graph_document: &serde_json::Value) -> BTreeSet<String> {
    graph_document["nodes"]
        .as_array()
        .expect("a document holds nodes")
        .iter()
        .filter(|node| {
            !ProcessorClassImportPath::new(node["type"].as_str().expect("a node has a type"))
                .expect("a type names a class")
                .names_a_built_in_node()
        })
        .map(|node| node["id"].as_str().expect("a node has a UUID").to_string())
        .collect()
}

/// A golden reads whole with a UUID on its asset and every node, is the canonical
/// document this build writes of it, and with its module nodes taken out by UUID, the
/// built-ins left load on a runtime with no interpreter.
#[test]
fn every_golden_graph_document_reads_canonically_and_loads_on_this_floor() {
    register_media_builtin_processor_types();

    for path in golden_graph_document_paths() {
        let document_bytes = std::fs::read(&path).expect("a golden document reads");
        let graph = GraphSnapshot::from_json_str(
            std::str::from_utf8(&document_bytes).expect("a golden document is UTF-8"),
        )
        .unwrap_or_else(|refusal| panic!("{path:?} no longer reads: {refusal}"));
        assert!(graph.asset.is_some(), "{path:?} holds no stream asset");
        assert!(
            graph.nodes.iter().all(|node| node.id.is_some()),
            "{path:?} holds a node with no UUID"
        );
        assert_eq!(
            String::from_utf8(graph.graph_document_bytes().expect("the graph writes")).unwrap(),
            String::from_utf8(document_bytes).unwrap(),
            "{path:?} is not the canonical document this build writes of it"
        );

        let recorded_graph_document = golden_graph_document(&path);
        let built_ins_only = GraphSnapshot::from_graph_document(without_the_nodes_of_the_uuids(
            &recorded_graph_document,
            &module_node_uuids_in(&recorded_graph_document),
        ))
        .unwrap_or_else(|refusal| {
            panic!("{path:?} without its module nodes no longer reads: {refusal}")
        });
        if built_ins_only.nodes.is_empty() {
            continue;
        }
        let project_directory = tempfile::tempdir().expect("a project directory");
        Runner::new()
            .expect("a runtime constructs")
            .load_stream_from_graph_snapshot(
                &built_ins_only,
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .with_stream_id("golden-document"),
            )
            .unwrap_or_else(|refusal| panic!("{path:?} no longer loads: {refusal}"));
    }
}

/// Making a red golden test green by editing the golden is the failure the record exists
/// to prevent, so an edit is red here whatever else passes.
#[test]
fn no_golden_graph_document_has_changed_or_gone_since_it_was_pinned() {
    let mut file_names_checked_in = BTreeSet::new();
    for path in golden_graph_document_paths() {
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("a golden document's file name is UTF-8")
            .to_string();
        let Some((_, pinned_sha256)) = GOLDEN_GRAPH_DOCUMENT_SHA256_BY_FILE_NAME
            .iter()
            .find(|(pinned_file_name, _)| *pinned_file_name == file_name)
        else {
            panic!(
                "{file_name} is pinned by no line of GOLDEN_GRAPH_DOCUMENT_SHA256_BY_FILE_NAME — pin it"
            );
        };
        let sha256: String = Sha256::digest(std::fs::read(&path).expect("a golden document reads"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();

        assert_eq!(
            sha256, *pinned_sha256,
            "{file_name} has changed since it was pinned. A golden is never edited: restore it \
             and add the new document as a golden beside it"
        );
        file_names_checked_in.insert(file_name);
    }
    for (pinned_file_name, _) in GOLDEN_GRAPH_DOCUMENT_SHA256_BY_FILE_NAME {
        assert!(
            file_names_checked_in.contains(*pinned_file_name),
            "{pinned_file_name} is pinned and no longer checked in"
        );
    }
}
