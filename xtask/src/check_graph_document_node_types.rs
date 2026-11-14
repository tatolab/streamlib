// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Holds every graph document's package node types to the module that defines each class.
//!
//! A node's type is its class's own `__module__:__qualname__`, and the stream's
//! interpreter refuses any other spelling at load. A package re-exports its nodes
//! from its top-level module, so `tatolab.camera:CameraSource` imports fine in a
//! REPL yet names a class that identifies as
//! `tatolab.camera.camera_source:CameraSource`. The package's
//! `[project.entry-points."tatolab.nodes"]` table states each defining path; a
//! graph-document type that names the same class through a parent module of that
//! path is the re-export, and fails here instead of on a rig.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The entry-point group a node package lists its node classes under.
const NODE_ENTRY_POINT_GROUP: &str = "tatolab.nodes";

/// The file-name suffix every stream's graph document carries.
const GRAPH_DOCUMENT_FILE_NAME_SUFFIX: &str = ".graph.json";

/// The directory of SHA-pinned compatibility goldens: never edited, and their
/// module nodes are taken out before they load, so their types are a record.
const GOLDEN_GRAPH_DOCUMENTS_DIRECTORY_NAME: &str = "golden_graph_documents";

/// One graph-document node whose type names a package node through a re-export.
#[derive(Debug, PartialEq, Eq)]
pub struct GraphDocumentNodeTypeNamingAReExport {
    /// Repository-relative path of the graph document.
    pub graph_document_path: String,
    /// The node's `name` in that document.
    pub node_name: String,
    /// The type the document spells.
    pub type_in_the_document: String,
    /// The defining import path the package's entry points state.
    pub defining_import_path: String,
}

/// What the gate read and what it found.
#[derive(Debug, Default)]
pub struct GraphDocumentNodeTypeScanReport {
    pub graph_documents_scanned: usize,
    pub node_entry_points_read: usize,
    pub node_types_naming_a_re_export: Vec<GraphDocumentNodeTypeNamingAReExport>,
}

/// Run the gate over every graph document in the repository.
pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-graph-document-node-types",
        "the repository's graph documents",
        report.graph_documents_scanned,
        "a graph document naming a node by a re-export",
    )?;
    crate::ensure_source_walking_gate_read_source(
        "check-graph-document-node-types",
        "the packages' `tatolab.nodes` entry points",
        report.node_entry_points_read,
        "a graph document naming a node by a re-export",
    )?;

    let failure_lines: Vec<String> = report
        .node_types_naming_a_re_export
        .iter()
        .map(|violation| {
            format!(
                "{}: node `{}` has type `{}`, a re-export; its class identifies as `{}`, \
                 the path the runtime loads it by",
                violation.graph_document_path,
                violation.node_name,
                violation.type_in_the_document,
                violation.defining_import_path,
            )
        })
        .collect();
    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-graph-document-node-types found {} violation(s):\n{}",
        failure_lines.len(),
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-graph-document-node-types: {} graph documents against {} node entry points",
        report.graph_documents_scanned,
        report.node_entry_points_read,
    );
    Ok(())
}

/// Read every package's node entry points, then every graph document's node types.
pub fn scan(workspace_root: &Path) -> Result<GraphDocumentNodeTypeScanReport> {
    let repository_paths = crate::list_repository_files_under(workspace_root, ".")?;

    let mut defining_import_paths: Vec<String> = Vec::new();
    for pyproject_path in repository_paths
        .iter()
        .filter(|repository_path| repository_path.ends_with("pyproject.toml"))
    {
        let pyproject_text = std::fs::read_to_string(workspace_root.join(pyproject_path))
            .with_context(|| format!("reading {pyproject_path}"))?;
        let pyproject: toml::Value =
            toml::from_str(&pyproject_text).with_context(|| format!("parsing {pyproject_path}"))?;
        let node_entry_points = pyproject
            .get("project")
            .and_then(|project| project.get("entry-points"))
            .and_then(|entry_points| entry_points.get(NODE_ENTRY_POINT_GROUP))
            .and_then(toml::Value::as_table);
        for import_path in node_entry_points
            .into_iter()
            .flat_map(|table| table.values())
            .filter_map(toml::Value::as_str)
        {
            defining_import_paths.push(import_path.to_owned());
        }
    }

    let defining_import_path_by_re_export =
        re_exports_of_the_defining_import_paths(&defining_import_paths);

    let mut report = GraphDocumentNodeTypeScanReport {
        node_entry_points_read: defining_import_paths.len(),
        ..Default::default()
    };
    for graph_document_path in repository_paths
        .iter()
        .filter(|repository_path| repository_path.ends_with(GRAPH_DOCUMENT_FILE_NAME_SUFFIX))
        .filter(|repository_path| {
            !Path::new(repository_path)
                .components()
                .any(|component| component.as_os_str() == GOLDEN_GRAPH_DOCUMENTS_DIRECTORY_NAME)
        })
    {
        let graph_document_text = std::fs::read_to_string(workspace_root.join(graph_document_path))
            .with_context(|| format!("reading {graph_document_path}"))?;
        let graph_document: serde_json::Value = serde_json::from_str(&graph_document_text)
            .with_context(|| format!("parsing {graph_document_path}"))?;
        report.graph_documents_scanned += 1;
        let nodes = graph_document
            .get("nodes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten();
        for node in nodes {
            let Some(type_in_the_document) = node.get("type").and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            if let Some(defining_import_path) =
                defining_import_path_by_re_export.get(type_in_the_document)
            {
                report
                    .node_types_naming_a_re_export
                    .push(GraphDocumentNodeTypeNamingAReExport {
                        graph_document_path: graph_document_path.clone(),
                        node_name: node
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        type_in_the_document: type_in_the_document.to_owned(),
                        defining_import_path: defining_import_path.clone(),
                    });
            }
        }
    }
    Ok(report)
}

/// Every `parent.module:Class` that re-exports a `parent.module.child:Class`, mapped to the latter.
fn re_exports_of_the_defining_import_paths(
    defining_import_paths: &[String],
) -> BTreeMap<String, String> {
    let mut defining_import_path_by_re_export = BTreeMap::new();
    for defining_import_path in defining_import_paths {
        let Some((defining_module, class_qualname)) = defining_import_path.split_once(':') else {
            continue;
        };
        let mut parent_module = defining_module;
        while let Some((grandparent_module, _)) = parent_module.rsplit_once('.') {
            parent_module = grandparent_module;
            defining_import_path_by_re_export.insert(
                format!("{parent_module}:{class_qualname}"),
                defining_import_path.clone(),
            );
        }
    }
    defining_import_path_by_re_export
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn workspace_with_a_camera_package_and_a_graph_document_naming(
        camera_node_type: &str,
    ) -> tempfile::TempDir {
        let workspace = tempfile::TempDir::new().unwrap();
        write_file(
            &workspace
                .path()
                .join("packages/tatolab-camera/pyproject.toml"),
            "[project]\nname = \"tatolab-camera\"\n\n\
             [project.entry-points.\"tatolab.nodes\"]\n\
             CameraSource = \"tatolab.camera.camera_source:CameraSource\"\n",
        );
        write_file(
            &workspace
                .path()
                .join("examples/camera-display/streams/main.graph.json"),
            &format!(
                "{{\"links\": [], \"nodes\": [\
                 {{\"config\": {{}}, \"id\": \"a\", \"name\": \"camerasource\", \"type\": \"{camera_node_type}\"}},\
                 {{\"config\": {{}}, \"id\": \"b\", \"name\": \"local\", \"type\": \"nodes.local_effect:LocalEffect\"}}\
                 ]}}"
            ),
        );
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        workspace
    }

    #[test]
    fn a_node_named_by_its_defining_module_passes() {
        let workspace = workspace_with_a_camera_package_and_a_graph_document_naming(
            "tatolab.camera.camera_source:CameraSource",
        );
        let report = scan(workspace.path()).unwrap();
        assert_eq!(report.graph_documents_scanned, 1);
        assert_eq!(report.node_entry_points_read, 1);
        assert!(report.node_types_naming_a_re_export.is_empty());
        run(workspace.path()).unwrap();
    }

    #[test]
    fn a_node_named_by_its_packages_re_export_fails_naming_the_defining_path() {
        let workspace = workspace_with_a_camera_package_and_a_graph_document_naming(
            "tatolab.camera:CameraSource",
        );
        let report = scan(workspace.path()).unwrap();
        assert_eq!(
            report.node_types_naming_a_re_export,
            vec![GraphDocumentNodeTypeNamingAReExport {
                graph_document_path: "examples/camera-display/streams/main.graph.json".to_owned(),
                node_name: "camerasource".to_owned(),
                type_in_the_document: "tatolab.camera:CameraSource".to_owned(),
                defining_import_path: "tatolab.camera.camera_source:CameraSource".to_owned(),
            }]
        );
        let failure = run(workspace.path()).unwrap_err().to_string();
        assert!(
            failure.contains("tatolab.camera.camera_source:CameraSource"),
            "{failure}"
        );
    }

    #[test]
    fn a_golden_compatibility_record_is_not_held_to_the_rule() {
        let workspace = workspace_with_a_camera_package_and_a_graph_document_naming(
            "tatolab.camera.camera_source:CameraSource",
        );
        write_file(
            &workspace
                .path()
                .join("runtime/some-crate/tests/golden_graph_documents/0001.graph.json"),
            "{\"links\": [], \"nodes\": [{\"id\": \"a\", \"name\": \"camerasource\", \
             \"type\": \"tatolab.camera:CameraSource\"}]}",
        );
        let report = scan(workspace.path()).unwrap();
        assert_eq!(report.graph_documents_scanned, 1);
        assert!(report.node_types_naming_a_re_export.is_empty());
    }

    #[test]
    fn a_class_defined_in_the_top_level_module_has_no_re_export() {
        let re_exports = re_exports_of_the_defining_import_paths(&[
            "tatolab.microphone:MicrophoneSource".to_owned(),
        ]);
        assert_eq!(
            re_exports
                .get("tatolab:MicrophoneSource")
                .map(String::as_str),
            Some("tatolab.microphone:MicrophoneSource")
        );
        assert!(!re_exports.contains_key("tatolab.microphone:MicrophoneSource"));
    }
}
