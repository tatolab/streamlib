// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    ProjectAssetAction, ProjectAssetImportSource, ProjectOperation, ProjectRelativeAssetPath,
    ProjectRepositoriesOpenInThisRuntime, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionLineage, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocumentNode,
    StreamGraphOperation, StreamGraphOperationOnAStream, graph_document_as_its_project_reads_it,
    the_file_of_an_imported_asset,
};
use uuid::Uuid;

mod assets_streams_and_node_types_inside_imports_tests;
mod importing_updating_and_removing_tests;
mod python_requirements_and_core_nodes_tests;

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "jo".to_owned(),
    }
}

/// A project of one stream, `streams/main.graph.json`.
struct ProjectWithOneStream {
    repository: ProjectRepository,
    stream: Uuid,
}

impl ProjectWithOneStream {
    fn created_in(root: &Path, name: &str) -> Self {
        let project_directory = root.join(name);
        ProjectRepository::create(
            &project_directory,
            name,
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let stream = Uuid::new_v4();
        std::fs::create_dir_all(project_directory.join("streams")).unwrap();
        std::fs::write(
            project_directory.join("streams/main.graph.json"),
            json!({ "asset": stream, "nodes": [], "links": [] }).to_string(),
        )
        .unwrap();
        Self {
            repository: ProjectRepository::open(&project_directory).unwrap(),
            stream,
        }
    }

    fn directory(&self) -> PathBuf {
        self.repository.project_directory().to_path_buf()
    }

    fn project(&self) -> Uuid {
        self.repository.project_file().project
    }

    fn add_a_node(&mut self, name: &str, node_type: &str, input_values: Value) -> Uuid {
        let Value::Object(input_values) = input_values else {
            panic!("a node's input values are an object");
        };
        let node = Uuid::new_v4();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                vec![ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: self.stream,
                        graph_operation: StreamGraphOperation::AddNode {
                            node: StreamGraphDocumentNode {
                                id: node,
                                name: name.to_owned(),
                                node_type: node_type.to_owned(),
                                input_values,
                                exposed_config_fields: Vec::new(),
                                studio: Default::default(),
                                fragment: None,
                            },
                            links: Vec::new(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap()
            .into_applied_transaction();
        node
    }

    fn import_a_lut(&mut self, bytes: &[u8]) -> Uuid {
        let source = self.directory().join("grade.cube");
        std::fs::write(&source, bytes).unwrap();
        let applied = self
            .repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::ImportAsset {
                    source: ProjectAssetImportSource::FileOnThisMachine(source),
                    asset_path: Some("grade.cube".to_owned()),
                    file_name: None,
                    kind: None,
                    import_settings: Map::new(),
                }],
            )
            .unwrap()
            .into_applied_transaction();
        self.repository
            .assets()
            .unwrap()
            .into_iter()
            .find(|asset| asset.path.as_str() == "grade.cube")
            .map(|asset| asset.asset)
            .unwrap_or_else(|| panic!("{applied:?} imported no grade.cube"))
    }

    fn undo(&mut self) {
        let reversal = self.repository.undo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .unwrap()
            .into_applied_transaction();
    }

    fn redo(&mut self) {
        let reversal = self.repository.redo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .unwrap()
            .into_applied_transaction();
    }

    fn main_document(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.directory().join("streams/main.graph.json")).unwrap(),
        )
        .unwrap()
    }
}

/// `directory` zipped into `archive`, every path under `top_folder/` when given, as a GitHub
/// archive wraps a repository.
fn zip_the_folder(directory: &Path, archive: &Path, top_folder: Option<&str>) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
    let mut directories = vec![directory.to_path_buf()];
    while let Some(reading) = directories.pop() {
        for entry in std::fs::read_dir(&reading).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                directories.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(directory)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let name = match top_folder {
                Some(top_folder) => format!("{top_folder}/{relative}"),
                None => relative,
            };
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer
                .write_all(&std::fs::read(entry.path()).unwrap())
                .unwrap();
        }
    }
    writer.finish().unwrap();
}

/// An arm module holding a stream of one node that reads its own LUT, and a cell that imports
/// it.
struct CellImportingArm {
    root: tempfile::TempDir,
    arm: ProjectWithOneStream,
    arm_lut: Uuid,
    cell: ProjectWithOneStream,
}

impl CellImportingArm {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut arm = ProjectWithOneStream::created_in(root.path(), "robot-arm");
        let arm_lut = arm.import_a_lut(b"LUT 1");
        arm.add_a_node(
            "grade",
            "nodes.arm:Grade",
            json!({ "lut": { "asset": arm_lut } }),
        );
        let cell = ProjectWithOneStream::created_in(root.path(), "cell");
        Self {
            root,
            arm,
            arm_lut,
            cell,
        }
    }

    fn import_the_arm(&mut self) {
        let source = self.arm.directory().display().to_string();
        self.cell
            .repository
            .add_dependency(&a_person(), None, &source, None)
            .unwrap()
            .into_applied_transaction();
    }

    fn place_the_arms_stream_as_a_fragment(&mut self) -> Uuid {
        self.cell.add_a_node(
            "arm",
            STREAM_FRAGMENT_NODE_TYPE,
            json!({ "stream": { "asset": self.arm.stream, "from": self.arm.project() } }),
        )
    }
}
