// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    ProjectAssetChangeRequest, ProjectAssetImportSource, ProjectOperation,
    ProjectRelativeAssetPath, ProjectRepositoriesOpenInThisRuntime, ProjectRepository,
    ProjectRepositoryRefusal, ProjectStreamDocumentOperation, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, STREAM_FRAGMENT_NODE_TYPE,
    StreamGraphDocumentNode, StreamGraphOperation, StreamGraphOperationOnAStream,
    graph_document_with_each_stream_fragment_reference_resolved, the_file_of_an_imported_asset,
};
use uuid::Uuid;

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

    fn add_a_node(&mut self, name: &str, node_type: &str, config: Value) -> Uuid {
        let node = Uuid::new_v4();
        self.repository
            .apply_project_operations(
                &a_person(),
                vec![ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: self.stream,
                        graph_operation: StreamGraphOperation::AddNode {
                            node: StreamGraphDocumentNode {
                                id: node,
                                name: name.to_owned(),
                                node_type: node_type.to_owned(),
                                config,
                                pins: Map::new(),
                                exposed_config_fields: Vec::new(),
                                studio: Map::new(),
                                fragment: None,
                            },
                            links: Vec::new(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap();
        node
    }

    fn import_a_lut(&mut self, bytes: &[u8]) -> Uuid {
        let source = self.directory().join("grade.cube");
        std::fs::write(&source, bytes).unwrap();
        let applied = self
            .repository
            .apply_asset_changes(
                &a_person(),
                vec![ProjectAssetChangeRequest::ImportAsset {
                    source: ProjectAssetImportSource::FileOnThisMachine(source),
                    asset_path: "grade.cube".to_owned(),
                    kind: None,
                    import_settings: Map::new(),
                }],
            )
            .unwrap();
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
            .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
            .unwrap();
    }

    fn redo(&mut self) {
        let reversal = self.repository.redo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
            .unwrap();
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
            .add_dependency(&a_person(), &source, None)
            .unwrap();
    }

    fn place_the_arms_stream_as_a_fragment(&mut self) -> Uuid {
        self.cell.add_a_node(
            "arm",
            STREAM_FRAGMENT_NODE_TYPE,
            json!({ "stream": { "asset": self.arm.stream, "from": self.arm.project() } }),
        )
    }
}

#[test]
fn a_module_is_copied_whole_into_its_imports_folder_and_recorded_with_its_version() {
    let mut imports = CellImportingArm::new();
    std::fs::create_dir_all(imports.arm.directory().join("__pycache__")).unwrap();
    std::fs::write(imports.arm.directory().join("__pycache__/x.pyc"), b"cache").unwrap();

    imports.import_the_arm();

    let import_folder = imports.cell.directory().join("imports/robot-arm");
    assert!(import_folder.join("tatolab-project.json").is_file());
    assert!(import_folder.join("streams/main.graph.json").is_file());
    assert_eq!(
        std::fs::read(import_folder.join("grade.cube")).unwrap(),
        b"LUT 1"
    );
    assert!(!import_folder.join(".tatolab").exists());
    assert!(!import_folder.join("__pycache__").exists());
    let import = imports
        .cell
        .repository
        .import(imports.arm.project())
        .unwrap();
    assert_eq!(import.folder, "robot-arm");
    assert_eq!(import.source, imports.arm.directory().display().to_string());
    assert!(import.copied_files.contains_key("grade.cube.asset.json"));
    assert_eq!(import.version.len(), 64);
    assert!(
        imports
            .cell
            .repository
            .assets()
            .unwrap()
            .iter()
            .all(|asset| !asset.path.as_str().starts_with("imports/")),
        "an import's assets are never the cell's own"
    );
    let recorded = imports.cell.repository.history().files_at_head().unwrap();
    assert!(recorded.contains_key("imports/robot-arm/streams/main.graph.json"));
    assert!(recorded.contains_key("imports/robot-arm/grade.cube.asset.json"));
    assert!(
        !recorded.contains_key("imports/robot-arm/grade.cube"),
        "history records an asset's file by its sidecar alone"
    );
}

#[test]
fn an_imported_asset_resolves_to_its_file_inside_the_import() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();

    let file = the_file_of_an_imported_asset(
        &imports.cell.directory(),
        imports.arm.project(),
        imports.arm_lut,
    )
    .unwrap();

    assert_eq!(
        file,
        imports
            .cell
            .directory()
            .join("imports/robot-arm/grade.cube")
    );
    std::fs::remove_file(&file).unwrap();
    let refusal = the_file_of_an_imported_asset(
        &imports.cell.directory(),
        imports.arm.project(),
        imports.arm_lut,
    )
    .unwrap_err();
    assert!(
        matches!(refusal, ProjectRepositoryRefusal::AssetFileMissing { ref file, .. } if file.ends_with("grade.cube")),
        "{refusal}"
    );
}

#[test]
fn a_zip_and_a_github_archive_of_a_module_are_imported_like_its_folder() {
    for top_folder in [None, Some("robot-arm-main")] {
        let mut imports = CellImportingArm::new();
        let archive = imports.root.path().join("robot-arm.zip");
        zip_the_folder(&imports.arm.directory(), &archive, top_folder);

        imports
            .cell
            .repository
            .add_dependency(&a_person(), &archive.display().to_string(), None)
            .unwrap();

        let import_folder = imports.cell.directory().join("imports/robot-arm");
        assert!(
            import_folder.join("streams/main.graph.json").is_file(),
            "{top_folder:?}"
        );
        assert!(!import_folder.join(".tatolab").exists(), "{top_folder:?}");
    }
}

#[test]
fn a_source_holding_no_project_file_or_no_project_uuid_is_refused_by_what_is_missing() {
    let mut imports = CellImportingArm::new();
    let not_a_project = imports.root.path().join("loose-files");
    std::fs::create_dir_all(&not_a_project).unwrap();
    std::fs::write(not_a_project.join("readme.txt"), b"hello").unwrap();

    let refusal = imports
        .cell
        .repository
        .add_dependency(&a_person(), &not_a_project.display().to_string(), None)
        .unwrap_err();
    assert!(
        refusal.to_string().contains("tatolab-project.json"),
        "{refusal}"
    );

    std::fs::write(
        not_a_project.join("tatolab-project.json"),
        b"{\"name\": \"x\"}",
    )
    .unwrap();
    let refusal = imports
        .cell
        .repository
        .add_dependency(&a_person(), &not_a_project.display().to_string(), None)
        .unwrap_err();
    assert!(refusal.to_string().contains("project UUID"), "{refusal}");
    assert!(!imports.cell.directory().join("imports").exists());
    assert!(
        std::fs::read_dir(imports.cell.directory().join(".tatolab/staged-imports"))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "a refused import leaves nothing staged"
    );
}

#[test]
fn a_project_is_refused_as_its_own_import_and_a_second_import_of_one_is_refused() {
    let mut imports = CellImportingArm::new();
    let cell_source = imports.cell.directory().display().to_string();
    assert!(matches!(
        imports
            .cell
            .repository
            .add_dependency(&a_person(), &cell_source, Some("me")),
        Err(ProjectRepositoryRefusal::ProjectImportsItself(_))
    ));
    imports.import_the_arm();
    let arm_source = imports.arm.directory().display().to_string();
    assert!(matches!(
        imports
            .cell
            .repository
            .add_dependency(&a_person(), &arm_source, Some("arm-again")),
        Err(ProjectRepositoryRefusal::ImportAlreadyHeld(_))
    ));
}

#[test]
fn a_fragment_holds_a_reference_and_each_load_reads_the_stream_it_names() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let fragment_node = imports.place_the_arms_stream_as_a_fragment();

    let stored = imports.cell.main_document();
    assert!(
        stored["nodes"][0].get("fragment").is_none(),
        "a stream fragment stores no nodes: {stored}"
    );
    let resolved = graph_document_with_each_stream_fragment_reference_resolved(
        &imports.cell.directory(),
        &stored,
    )
    .unwrap();
    let placed = &resolved["nodes"][0]["fragment"]["nodes"][0];
    assert_eq!(placed["name"], "grade");
    let arm_node = imports.arm.main_document()["nodes"][0]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    assert_eq!(
        placed["id"],
        Uuid::new_v5(&fragment_node, arm_node.as_bytes()).to_string()
    );
    assert_eq!(
        placed["config"]["lut"],
        json!({ "asset": imports.arm_lut, "from": imports.arm.project() })
    );

    let imported_stream = imports
        .cell
        .directory()
        .join("imports/robot-arm/streams/main.graph.json");
    let mut edited: Value =
        serde_json::from_slice(&std::fs::read(&imported_stream).unwrap()).unwrap();
    edited["nodes"][0]["name"] = json!("grade-edited");
    std::fs::write(&imported_stream, edited.to_string()).unwrap();
    let reloaded = graph_document_with_each_stream_fragment_reference_resolved(
        &imports.cell.directory(),
        &stored,
    )
    .unwrap();
    assert_eq!(
        reloaded["nodes"][0]["fragment"]["nodes"][0]["name"], "grade-edited",
        "an edit of the module's stream changes what the next load reads"
    );
}

#[test]
fn a_stream_that_holds_itself_through_another_is_refused_at_load_by_name() {
    let root = tempfile::tempdir().unwrap();
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let other = Uuid::new_v4();
    std::fs::write(
        cell.directory().join("streams/other.graph.json"),
        json!({ "asset": other, "links": [], "nodes": [{
            "id": Uuid::new_v4(), "name": "back", "type": STREAM_FRAGMENT_NODE_TYPE,
            "config": { "stream": { "asset": cell.stream } } }] })
        .to_string(),
    )
    .unwrap();
    cell.repository = ProjectRepository::open(&cell.directory()).unwrap();
    cell.add_a_node(
        "other",
        STREAM_FRAGMENT_NODE_TYPE,
        json!({ "stream": { "asset": other } }),
    );

    let refusal = graph_document_with_each_stream_fragment_reference_resolved(
        &cell.directory(),
        &cell.main_document(),
    )
    .unwrap_err();

    assert!(refusal.contains("cycle"), "{refusal}");
    assert!(refusal.contains("`other`"), "{refusal}");
}

#[test]
fn an_import_is_removed_only_once_nothing_references_it_and_undo_puts_it_back() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let arm = imports.arm.project();
    imports.place_the_arms_stream_as_a_fragment();

    let refusal = imports
        .cell
        .repository
        .remove_dependency(&a_person(), arm)
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ImportStillReferenced { .. }
        ),
        "{refusal}"
    );

    imports.cell.undo();
    imports
        .cell
        .repository
        .remove_dependency(&a_person(), arm)
        .unwrap();
    let import_folder = imports.cell.directory().join("imports/robot-arm");
    assert!(!import_folder.exists());
    assert!(imports.cell.repository.import(arm).is_err());

    imports.cell.undo();
    assert!(import_folder.join("streams/main.graph.json").is_file());
    assert!(import_folder.join("grade.cube.asset.json").is_file());
    assert!(
        !import_folder.join("grade.cube").exists(),
        "an asset's file history never held reads as missing after an undo"
    );
    assert!(imports.cell.repository.import(arm).is_ok());
}

#[test]
fn undo_of_an_import_takes_its_folder_out_and_redo_puts_it_back_from_history() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let import_folder = imports.cell.directory().join("imports/robot-arm");

    imports.cell.undo();
    assert!(!import_folder.exists());
    assert!(
        imports
            .cell
            .repository
            .project_file()
            .dependencies
            .is_empty()
    );

    imports.cell.redo();
    assert!(import_folder.join("streams/main.graph.json").is_file());
    assert_eq!(imports.cell.repository.project_file().dependencies.len(), 1);
}

#[test]
fn an_update_over_edited_files_answers_them_and_overwrites_once_confirmed() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let arm = imports.arm.project();
    imports.arm.add_a_node("tint", "nodes.arm:Tint", json!({}));
    let edited_in_the_cell = imports
        .cell
        .directory()
        .join("imports/robot-arm/streams/main.graph.json");
    std::fs::write(&edited_in_the_cell, b"{\"asset\": \"edited\"}").unwrap();

    let refusal = imports
        .cell
        .repository
        .update_dependency(&a_person(), arm, None, false)
        .unwrap_err();
    match refusal {
        ProjectRepositoryRefusal::ImportEditedSinceItsCopy { edited_files, .. } => {
            assert_eq!(edited_files, vec!["streams/main.graph.json".to_owned()]);
        }
        refusal => panic!("{refusal}"),
    }

    let version_before = imports.cell.repository.import(arm).unwrap().version;
    imports
        .cell
        .repository
        .update_dependency(&a_person(), arm, None, true)
        .unwrap();
    let updated: Value =
        serde_json::from_slice(&std::fs::read(&edited_in_the_cell).unwrap()).unwrap();
    assert_eq!(updated["nodes"][1]["name"], "tint");
    assert_ne!(
        imports.cell.repository.import(arm).unwrap().version,
        version_before
    );
    assert!(
        imports
            .cell
            .repository
            .files_edited_in_the_import(&imports.cell.repository.import(arm).unwrap())
            .unwrap()
            .is_empty()
    );

    imports.cell.undo();
    assert_eq!(
        imports.cell.repository.import(arm).unwrap().version,
        version_before
    );
}

#[test]
fn a_listing_checked_for_newer_answers_the_version_the_source_holds_now() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let recorded = imports.cell.repository.import_listing(true);
    assert_eq!(
        recorded[0].version_on_the_source,
        Some(Ok(recorded[0].import.version.clone()))
    );

    imports.arm.add_a_node("tint", "nodes.arm:Tint", json!({}));
    let listed = imports.cell.repository.import_listing(true);
    assert!(
        matches!(&listed[0].version_on_the_source, Some(Ok(newer)) if *newer != listed[0].import.version)
    );
    assert_eq!(listed[0].edited_files, Ok(Vec::new()));
    assert_eq!(
        imports.cell.repository.import_listing(false)[0].version_on_the_source,
        None
    );
}

#[test]
fn a_project_with_a_module_zipped_and_unzipped_elsewhere_reads_the_modules_stream_and_asset() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    imports.place_the_arms_stream_as_a_fragment();
    let archive = imports.root.path().join("cell.zip");
    zip_the_folder(&imports.cell.directory(), &archive, None);
    let elsewhere = tempfile::tempdir().unwrap();
    let mut unzipped = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
    unzipped.extract(elsewhere.path()).unwrap();
    std::fs::remove_dir_all(elsewhere.path().join(".tatolab")).unwrap();
    std::fs::remove_dir_all(imports.arm.directory()).unwrap();

    let reopened = ProjectRepository::open(elsewhere.path()).unwrap();
    let resolved = graph_document_with_each_stream_fragment_reference_resolved(
        elsewhere.path(),
        &reopened
            .stream_graph_document(imports.cell.stream)
            .unwrap()
            .to_value(),
    )
    .unwrap();

    assert_eq!(
        resolved["nodes"][0]["fragment"]["nodes"][0]["name"],
        "grade"
    );
    assert_eq!(
        std::fs::read(
            the_file_of_an_imported_asset(elsewhere.path(), imports.arm.project(), imports.arm_lut)
                .unwrap()
        )
        .unwrap(),
        b"LUT 1"
    );
}

#[test]
fn a_transaction_in_an_import_opened_as_its_own_project_commits_on_the_enclosing_project_too() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let cell_commits_before = imports
        .cell
        .repository
        .history()
        .commits_newest_first()
        .unwrap()
        .len();
    let import_folder = imports.cell.directory().join("imports/robot-arm");
    let open_in_this_runtime = ProjectRepositoriesOpenInThisRuntime::default();
    let module = open_in_this_runtime
        .the_project_repository_at(&import_folder)
        .unwrap();
    let mut module = module.lock();

    let applied = module
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: imports.arm.stream,
                    graph_operation: StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: Uuid::new_v4(),
                            name: "tint".to_owned(),
                            node_type: "nodes.arm:Tint".to_owned(),
                            config: json!({}),
                            pins: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Map::new(),
                            fragment: None,
                        },
                        links: Vec::new(),
                    },
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    open_in_this_runtime.announce_applied_transaction(&module, applied);

    assert!(module.history().commits_newest_first().unwrap().len() >= 2);
    let cell = ProjectRepository::open(&imports.cell.directory()).unwrap();
    let cell_commits = cell.history().commits_newest_first().unwrap();
    assert_eq!(cell_commits.len(), cell_commits_before + 1);
    assert!(
        cell_commits[0]
            .message
            .starts_with("edit imports/robot-arm"),
        "{}",
        cell_commits[0].message
    );
    let recorded: Value = serde_json::from_slice(
        &cell.history().files_at_head().unwrap()["imports/robot-arm/streams/main.graph.json"],
    )
    .unwrap();
    assert_eq!(recorded["nodes"][1]["name"], "tint");
    assert_eq!(
        imports.arm.main_document()["nodes"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "nothing is written back to the source"
    );
}

#[test]
fn an_asset_path_inside_imports_is_refused() {
    assert!(ProjectRelativeAssetPath::parse("imports/robot-arm/grade.cube").is_err());
    assert!(ProjectRelativeAssetPath::parse("media/imports/grade.cube").is_ok());
}

#[test]
fn a_created_stream_document_referencing_an_asset_the_project_does_not_hold_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let stream = Uuid::new_v4();
    let missing_asset = Uuid::new_v4();
    let nested_node = Uuid::new_v4();

    let refusal = cell
        .repository
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument {
                    stream,
                    path: ProjectRelativeAssetPath::parse("streams/made.graph.json").unwrap(),
                    document: json!({ "asset": stream, "links": [], "nodes": [{
                        "id": Uuid::new_v4(), "name": "generated", "type": "generators:Look",
                        "config": {}, "fragment": { "links": [], "nodes": [{
                            "id": nested_node, "name": "lut", "type": "nodes:Lut",
                            "config": { "lut": { "asset": missing_asset } } }] } }] }),
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { node, asset }
                if node == nested_node && asset == missing_asset
        ),
        "{refusal}"
    );
    assert!(!cell.directory().join("streams/made.graph.json").exists());
}

#[test]
fn an_import_recorded_under_a_folder_outside_imports_is_passed_over() {
    let root = tempfile::tempdir().unwrap();
    let cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let escaping_project = Uuid::new_v4();
    let mut project_file: Value = serde_json::from_slice(
        &std::fs::read(cell.directory().join("tatolab-project.json")).unwrap(),
    )
    .unwrap();
    project_file["dependencies"] = json!([
        { "project": escaping_project, "folder": "/", "source": "/", "version": "", "copied_files": {} },
        { "project": escaping_project, "folder": "../outside", "source": "/", "version": "", "copied_files": {} },
    ]);
    std::fs::write(
        cell.directory().join("tatolab-project.json"),
        project_file.to_string(),
    )
    .unwrap();

    assert!(matches!(
        streamlib_project_repository::the_folder_of_an_import(&cell.directory(), escaping_project),
        Err(ProjectRepositoryRefusal::NoSuchImport(project)) if project == escaping_project
    ));
}

#[test]
fn finder_metadata_is_never_copied_nor_answered_as_an_edit() {
    let mut imports = CellImportingArm::new();
    std::fs::write(imports.arm.directory().join("streams/.DS_Store"), b"finder").unwrap();
    imports.import_the_arm();
    let arm = imports.arm.project();
    let import_folder = imports.cell.directory().join("imports/robot-arm");
    assert!(!import_folder.join("streams/.DS_Store").exists());

    std::fs::write(import_folder.join(".DS_Store"), b"finder").unwrap();
    let import = imports.cell.repository.import(arm).unwrap();
    assert!(
        imports
            .cell
            .repository
            .files_edited_in_the_import(&import)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_zipped_module_holding_pyvenv_cfg_at_its_root_is_still_copied() {
    let mut imports = CellImportingArm::new();
    std::fs::write(imports.arm.directory().join("pyvenv.cfg"), b"home = /usr").unwrap();
    let archive = imports.root.path().join("robot-arm.zip");
    zip_the_folder(&imports.arm.directory(), &archive, Some("robot-arm-main"));

    imports
        .cell
        .repository
        .add_dependency(&a_person(), &archive.display().to_string(), None)
        .unwrap();

    assert!(
        imports
            .cell
            .directory()
            .join("imports/robot-arm/streams/main.graph.json")
            .is_file()
    );
}

#[test]
fn an_asset_only_a_nested_copy_of_an_import_holds_resolves_to_that_copy() {
    let root = tempfile::tempdir().unwrap();
    let mut gripper = ProjectWithOneStream::created_in(root.path(), "gripper");
    let mut arm = ProjectWithOneStream::created_in(root.path(), "robot-arm");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let add = |importing: &mut ProjectWithOneStream, imported: &ProjectWithOneStream| {
        importing
            .repository
            .add_dependency(
                &a_person(),
                &imported.directory().display().to_string(),
                None,
            )
            .unwrap();
    };
    add(&mut cell, &gripper);
    let lut_only_the_newer_gripper_holds = gripper.import_a_lut(b"LUT 2");
    add(&mut arm, &gripper);
    add(&mut cell, &arm);

    let file = the_file_of_an_imported_asset(
        &cell.directory(),
        gripper.project(),
        lut_only_the_newer_gripper_holds,
    )
    .unwrap();

    assert_eq!(
        file,
        cell.directory()
            .join("imports/robot-arm/imports/gripper/grade.cube")
    );
}

#[cfg(unix)]
#[test]
fn an_import_folder_linked_back_to_its_enclosing_project_ends_the_search() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let arm_folder = imports.cell.directory().join("imports/robot-arm");
    std::fs::create_dir_all(arm_folder.join("imports")).unwrap();
    std::os::unix::fs::symlink(imports.cell.directory(), arm_folder.join("imports/loop")).unwrap();
    let mut arm_project_file: Value =
        serde_json::from_slice(&std::fs::read(arm_folder.join("tatolab-project.json")).unwrap())
            .unwrap();
    arm_project_file["dependencies"] = json!([{
        "project": imports.cell.project(),
        "folder": "loop",
        "source": "/elsewhere",
        "version": "0".repeat(64),
        "copied_files": {},
    }]);
    std::fs::write(
        arm_folder.join("tatolab-project.json"),
        serde_json::to_vec(&arm_project_file).unwrap(),
    )
    .unwrap();

    let refusal = streamlib_project_repository::the_folder_of_an_import(
        &imports.cell.directory(),
        Uuid::new_v4(),
    )
    .unwrap_err();

    assert!(
        matches!(refusal, ProjectRepositoryRefusal::NoSuchImport(_)),
        "{refusal}"
    );
}

#[test]
fn a_node_type_an_imports_own_code_defines_is_led_by_its_folder_at_every_depth() {
    let root = tempfile::tempdir().unwrap();
    let write_node_code = |project: &ProjectWithOneStream, module_file: &str| {
        let module_path = project.directory().join(module_file);
        std::fs::create_dir_all(module_path.parent().unwrap()).unwrap();
        std::fs::write(module_path, "class Node: ...\n").unwrap();
    };
    let mut gripper = ProjectWithOneStream::created_in(root.path(), "gripper");
    write_node_code(&gripper, "nodes/gripper.py");
    gripper.add_a_node("grip", "nodes.gripper:Gripper", json!({}));
    let mut arm = ProjectWithOneStream::created_in(root.path(), "robot-arm");
    write_node_code(&arm, "nodes/arm.py");
    write_node_code(&arm, "arm_helpers.py");
    arm.add_a_node("grade", "nodes.arm:Grade", json!({}));
    arm.add_a_node("helped", "arm_helpers:Helped", json!({}));
    arm.add_a_node("pattern", "tatolab.testpattern:TestPattern", json!({}));
    arm.add_a_node("display", "tatolab.stream:Display", json!({}));
    arm.repository
        .add_dependency(
            &a_person(),
            &gripper.directory().display().to_string(),
            None,
        )
        .unwrap();
    arm.add_a_node(
        "claw",
        STREAM_FRAGMENT_NODE_TYPE,
        json!({ "stream": { "asset": gripper.stream, "from": gripper.project() } }),
    );
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    write_node_code(&cell, "nodes/cell.py");
    cell.add_a_node("own", "nodes.cell:Own", json!({}));
    cell.repository
        .add_dependency(&a_person(), &arm.directory().display().to_string(), None)
        .unwrap();
    cell.add_a_node(
        "arm",
        STREAM_FRAGMENT_NODE_TYPE,
        json!({ "stream": { "asset": arm.stream, "from": arm.project() } }),
    );

    let resolved = graph_document_with_each_stream_fragment_reference_resolved(
        &cell.directory(),
        &cell.main_document(),
    )
    .unwrap();

    let type_named = |level: &Value, name: &str| -> Value {
        level["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["name"] == name)
            .unwrap_or_else(|| panic!("no node `{name}` in {level}"))["type"]
            .clone()
    };
    assert_eq!(type_named(&resolved, "own"), "nodes.cell:Own");
    let arm_fragment = &resolved["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "arm")
        .unwrap()["fragment"];
    assert_eq!(
        type_named(arm_fragment, "grade"),
        "imports/robot-arm/nodes.arm:Grade"
    );
    assert_eq!(
        type_named(arm_fragment, "helped"),
        "imports/robot-arm/arm_helpers:Helped"
    );
    assert_eq!(
        type_named(arm_fragment, "pattern"),
        "tatolab.testpattern:TestPattern",
        "a type the import's own code does not define resolves in the project's venv"
    );
    assert_eq!(
        type_named(arm_fragment, "display"),
        "tatolab.stream:Display"
    );
    assert_eq!(type_named(arm_fragment, "claw"), STREAM_FRAGMENT_NODE_TYPE);
    let claw_fragment = &arm_fragment["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "claw")
        .unwrap()["fragment"];
    assert_eq!(
        type_named(claw_fragment, "grip"),
        "imports/robot-arm/imports/gripper/nodes.gripper:Gripper"
    );
    assert_eq!(
        cell.main_document()["nodes"][1]["config"],
        json!({ "stream": { "asset": arm.stream, "from": arm.project() } }),
        "the stored document keeps its reference; nothing is written back"
    );
}

#[test]
fn an_import_is_a_uv_path_source_of_the_project_through_undo_and_redo() {
    let mut imports = CellImportingArm::new();
    std::fs::write(
        imports.arm.directory().join("pyproject.toml"),
        "[project]\nname = \"robot-arm\"\nversion = \"0.1.0\"\ndependencies = [\"numpy>=2.1\"]\n",
    )
    .unwrap();
    let cell_pyproject = imports.cell.directory().join("pyproject.toml");
    std::fs::write(
        &cell_pyproject,
        "[project]\nname = \"cell\"\nversion = \"0.1.0\"\ndependencies = [\"tatolab-stream>=0.68\"]\n",
    )
    .unwrap();
    let the_cell_pyproject = || -> toml::Value {
        toml::from_str(&std::fs::read_to_string(&cell_pyproject).unwrap()).unwrap()
    };

    imports.import_the_arm();
    let imported = the_cell_pyproject();
    assert_eq!(
        imported["project"]["dependencies"],
        toml::Value::try_from(["tatolab-stream>=0.68", "robot-arm"]).unwrap()
    );
    assert_eq!(
        imported["tool"]["uv"]["sources"]["robot-arm"]["path"].as_str(),
        Some("imports/robot-arm")
    );
    assert_eq!(
        imported["tool"]["uv"]["sources"]["robot-arm"]["package"].as_bool(),
        Some(false)
    );

    imports.cell.undo();
    let undone = the_cell_pyproject();
    assert_eq!(
        undone["project"]["dependencies"],
        toml::Value::try_from(["tatolab-stream>=0.68"]).unwrap()
    );
    assert!(undone.get("tool").is_none(), "{undone}");

    imports.cell.redo();
    assert_eq!(the_cell_pyproject(), imported);
}

/// The cell and the arm, each with a `pyproject.toml`, the cell with a `uv.lock`.
fn cell_and_arm_with_python_requirements() -> CellImportingArm {
    let imports = CellImportingArm::new();
    std::fs::write(
        imports.arm.directory().join("pyproject.toml"),
        "[project]\nname = \"robot-arm\"\nversion = \"0.1.0\"\ndependencies = [\"numpy<1\"]\n",
    )
    .unwrap();
    std::fs::write(
        imports.cell.directory().join("pyproject.toml"),
        "[project]\nname = \"cell\"\nversion = \"0.1.0\"\ndependencies = [\"numpy>=2.1\"]\n",
    )
    .unwrap();
    std::fs::write(
        imports.cell.directory().join("uv.lock"),
        "the lock before\n",
    )
    .unwrap();
    imports
}

#[test]
fn an_import_whose_requirements_cannot_be_resolved_is_refused_in_the_resolvers_words() {
    let mut imports = cell_and_arm_with_python_requirements();
    let cell_directory = imports.cell.directory();
    let pyproject_before = std::fs::read_to_string(cell_directory.join("pyproject.toml")).unwrap();
    let commits_before = imports
        .cell
        .repository
        .applied_transactions_newest_first(8)
        .unwrap();
    imports
        .cell
        .repository
        .lock_the_python_requirements_with(Arc::new(|project_directory: &Path| {
            std::fs::write(project_directory.join("uv.lock"), "half written\n").unwrap();
            Err("No solution found when resolving dependencies: numpy>=2.1 and numpy<1".to_owned())
        }));
    let source = imports.arm.directory().display().to_string();

    let refusal = imports
        .cell
        .repository
        .add_dependency(&a_person(), &source, None)
        .unwrap_err();

    assert!(
        matches!(
            &refusal,
            ProjectRepositoryRefusal::ImportRequirementsUnresolvable { resolver_refusal }
                if resolver_refusal.contains("numpy>=2.1 and numpy<1")
        ),
        "{refusal}"
    );
    assert_eq!(
        std::fs::read_to_string(cell_directory.join("pyproject.toml")).unwrap(),
        pyproject_before
    );
    assert_eq!(
        std::fs::read_to_string(cell_directory.join("uv.lock")).unwrap(),
        "the lock before\n"
    );
    assert!(!cell_directory.join("imports/robot-arm").exists());
    assert!(
        imports
            .cell
            .repository
            .project_file()
            .dependencies
            .is_empty()
    );
    assert_eq!(
        imports
            .cell
            .repository
            .applied_transactions_newest_first(8)
            .unwrap(),
        commits_before
    );
}

#[test]
fn the_lock_an_import_writes_is_committed_with_the_import() {
    let mut imports = cell_and_arm_with_python_requirements();
    imports
        .cell
        .repository
        .lock_the_python_requirements_with(Arc::new(|project_directory: &Path| {
            std::fs::write(project_directory.join("uv.lock"), "locked with robot-arm\n").unwrap();
            Ok(())
        }));

    imports.import_the_arm();

    let files_at_head = imports.cell.repository.history().files_at_head().unwrap();
    assert_eq!(
        files_at_head.get("uv.lock").map(Vec::as_slice),
        Some(b"locked with robot-arm\n".as_slice())
    );
    assert!(
        String::from_utf8_lossy(&files_at_head["pyproject.toml"]).contains("imports/robot-arm")
    );
}

#[test]
fn a_project_the_runtime_opens_is_handed_its_lock() {
    let imports = cell_and_arm_with_python_requirements();
    let open = ProjectRepositoriesOpenInThisRuntime::locking_python_requirements_with(Arc::new(
        |_: &Path| Err("No solution found".to_owned()),
    ));
    let cell = open
        .the_project_repository_at(&imports.cell.directory())
        .unwrap();
    let source = imports.arm.directory().display().to_string();

    let refusal = cell
        .lock()
        .add_dependency(&a_person(), &source, None)
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ImportRequirementsUnresolvable { .. }
        ),
        "{refusal}"
    );
}
