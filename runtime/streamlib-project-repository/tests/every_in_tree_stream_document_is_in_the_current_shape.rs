// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Every stream document checked into this tree is written in the shape this runtime writes, so
//! the upgrade finds nothing to change in it. The golden documents, which record older shapes on
//! purpose, are the one exception.

use std::path::{Path, PathBuf};

/// Directories the walk does not enter: build output, installed packages, and the golden
/// documents that keep older shapes on purpose.
const DIRECTORIES_LEFT_OUT: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "golden_graph_documents",
];

fn every_stream_document_under(directory: &Path, found: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|refusal| panic!("{directory:?} does not list: {refusal}"));
    for entry in entries {
        let path = entry.expect("a directory entry reads").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !path.is_symlink() && !DIRECTORIES_LEFT_OUT.contains(&name) {
                every_stream_document_under(&path, found);
            }
        } else if name.ends_with(".graph.json") {
            found.push(path);
        }
    }
}

#[test]
fn the_upgrade_finds_nothing_to_change_in_any_in_tree_stream_document() {
    let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut documents = Vec::new();
    every_stream_document_under(&tree, &mut documents);
    assert!(!documents.is_empty(), "no stream document under {tree:?}");
    let in_an_old_shape: Vec<_> = documents
        .iter()
        .filter(|path| {
            let mut document: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(path).expect("a stream document reads"),
            )
            .unwrap_or_else(|refusal| panic!("{path:?} is not JSON: {refusal}"));
            streamlib_project_repository::upgrade_stream_document_in_place(&mut document)
        })
        .collect();
    assert!(
        in_an_old_shape.is_empty(),
        "these stream documents are in an older shape; open each in the runtime and save it, \
         so the upgrade rewrites it: {in_an_old_shape:#?}"
    );
}

#[test]
fn every_in_tree_link_onto_a_placed_stream_names_its_port_by_the_interface_nodes_uuid() {
    let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut documents = Vec::new();
    every_stream_document_under(&tree, &mut documents);
    let mut linked_by_a_port_name = Vec::new();
    for path in &documents {
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("a stream document reads"))
                .unwrap_or_else(|refusal| panic!("{path:?} is not JSON: {refusal}"));
        let placing_nodes: Vec<&str> = document["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| node["type"] == streamlib_project_repository::STREAM_FRAGMENT_NODE_TYPE)
            .filter_map(|node| node["id"].as_str())
            .collect();
        for link in document["links"].as_array().into_iter().flatten() {
            for end in [&link["source"], &link["target"]] {
                let on_a_placing_node = end["node"]
                    .as_str()
                    .is_some_and(|node| placing_nodes.contains(&node));
                let port = end["port"].as_str().unwrap_or_default();
                if on_a_placing_node && uuid::Uuid::parse_str(port).is_err() {
                    linked_by_a_port_name.push(format!("{}: {port}", path.display()));
                }
            }
        }
    }
    assert!(
        linked_by_a_port_name.is_empty(),
        "these links onto a placed stream name its port by name rather than by the UUID of its \
         `GraphInput` or `GraphOutput` node: {linked_by_a_port_name:#?}"
    );
}
