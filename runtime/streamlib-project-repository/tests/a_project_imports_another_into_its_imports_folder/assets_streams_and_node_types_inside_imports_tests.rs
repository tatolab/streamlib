// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What resolves inside an import: its assets, its streams opened as their own
//! project, the paths refused or passed over, the files never copied, and the
//! node types its own code defines.

use super::*;

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
            None,
            Vec::new(),
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
            None,
            Vec::new(),
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
        .add_dependency(&a_person(), None, &archive.display().to_string(), None)
        .unwrap()
        .into_applied_transaction();

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
                None,
                &imported.directory().display().to_string(),
                None,
            )
            .unwrap()
            .into_applied_transaction();
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
            None,
            &gripper.directory().display().to_string(),
            None,
        )
        .unwrap()
        .into_applied_transaction();
    arm.add_a_node(
        "claw",
        STREAM_FRAGMENT_NODE_TYPE,
        json!({ "stream": { "asset": gripper.stream, "from": gripper.project() } }),
    );
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    write_node_code(&cell, "nodes/cell.py");
    cell.add_a_node("own", "nodes.cell:Own", json!({}));
    cell.repository
        .add_dependency(
            &a_person(),
            None,
            &arm.directory().display().to_string(),
            None,
        )
        .unwrap()
        .into_applied_transaction();
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
