// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A module copied into a project's imports folder from a folder, a zip or a
//! GitHub archive, what the copy carries and leaves out, the sources refused,
//! and an import updated, removed, undone and redone.

use super::*;

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
fn a_zip_and_a_github_archive_of_a_module_are_imported_like_its_folder() {
    for top_folder in [None, Some("robot-arm-main")] {
        let mut imports = CellImportingArm::new();
        let archive = imports.root.path().join("robot-arm.zip");
        zip_the_folder(&imports.arm.directory(), &archive, top_folder);

        imports
            .cell
            .repository
            .add_dependency(&a_person(), None, &archive.display().to_string(), None)
            .unwrap()
            .into_applied_transaction();

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
        .add_dependency(
            &a_person(),
            None,
            &not_a_project.display().to_string(),
            None,
        )
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
        .add_dependency(
            &a_person(),
            None,
            &not_a_project.display().to_string(),
            None,
        )
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
            .add_dependency(&a_person(), None, &cell_source, Some("me")),
        Err(ProjectRepositoryRefusal::ProjectImportsItself(_))
    ));
    imports.import_the_arm();
    let arm_source = imports.arm.directory().display().to_string();
    assert!(matches!(
        imports
            .cell
            .repository
            .add_dependency(&a_person(), None, &arm_source, Some("arm-again")),
        Err(ProjectRepositoryRefusal::ImportAlreadyHeld(_))
    ));
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
        .remove_dependency(&a_person(), None, arm)
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
        .remove_dependency(&a_person(), None, arm)
        .unwrap()
        .into_applied_transaction();
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
        .update_dependency(&a_person(), None, arm, None, false)
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
        .update_dependency(&a_person(), None, arm, None, true)
        .unwrap()
        .into_applied_transaction();
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
