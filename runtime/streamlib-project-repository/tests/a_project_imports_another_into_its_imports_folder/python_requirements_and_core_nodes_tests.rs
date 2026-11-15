// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An import's Python requirements and lock, and the core nodes copy and link
//! an import never carries as its own.

use super::*;

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
        .add_dependency(&a_person(), None, &source, None)
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
        .add_dependency(&a_person(), None, &source, None)
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ImportRequirementsUnresolvable { .. }
        ),
        "{refusal}"
    );
}

#[test]
fn a_copy_of_core_nodes_inside_the_source_is_never_copied_from_a_folder_or_a_zip() {
    for from_a_zip in [false, true] {
        let mut imports = CellImportingArm::new();
        let core_nodes_in_the_arm = imports.arm.directory().join("imports/core-nodes");
        std::fs::create_dir_all(core_nodes_in_the_arm.join("core_nodes")).unwrap();
        std::fs::write(core_nodes_in_the_arm.join("pyproject.toml"), b"[project]\n").unwrap();
        std::fs::write(core_nodes_in_the_arm.join("core_nodes/color.py"), b"").unwrap();
        let source = if from_a_zip {
            let archive = imports.root.path().join("robot-arm.zip");
            zip_the_folder(&imports.arm.directory(), &archive, None);
            archive
        } else {
            imports.arm.directory()
        };

        imports
            .cell
            .repository
            .add_dependency(&a_person(), None, &source.display().to_string(), None)
            .unwrap()
            .into_applied_transaction();

        let import_folder = imports.cell.directory().join("imports/robot-arm");
        assert!(import_folder.join("streams/main.graph.json").is_file());
        assert!(
            !import_folder.join("imports").exists(),
            "from_a_zip: {from_a_zip}"
        );
        let import = imports
            .cell
            .repository
            .import(imports.arm.project())
            .unwrap();
        assert!(
            import
                .copied_files
                .keys()
                .all(|path| !path.starts_with("imports/")),
            "from_a_zip: {from_a_zip}"
        );
    }
}

#[test]
fn the_link_to_core_nodes_the_runtime_makes_in_an_import_is_no_edit_of_it() {
    let mut imports = CellImportingArm::new();
    imports.import_the_arm();
    let core_nodes_the_runtime_carries = imports.root.path().join("unit/core-nodes");
    std::fs::create_dir_all(&core_nodes_the_runtime_carries).unwrap();
    std::fs::write(
        core_nodes_the_runtime_carries.join("tatolab-project.json"),
        std::fs::read(imports.arm.directory().join("tatolab-project.json")).unwrap(),
    )
    .unwrap();
    let core_nodes_project = Uuid::new_v4();
    let mut core_nodes_project_file: Value = serde_json::from_slice(
        &std::fs::read(core_nodes_the_runtime_carries.join("tatolab-project.json")).unwrap(),
    )
    .unwrap();
    core_nodes_project_file["project"] = json!(core_nodes_project);
    std::fs::write(
        core_nodes_the_runtime_carries.join("tatolab-project.json"),
        core_nodes_project_file.to_string(),
    )
    .unwrap();

    streamlib_project_repository::link_the_core_project_into_each_import_of(
        &imports.cell.directory(),
        &core_nodes_the_runtime_carries,
    )
    .unwrap();

    let import_folder = imports.cell.directory().join("imports/robot-arm");
    assert_eq!(
        std::fs::read_link(import_folder.join("imports/core-nodes")).unwrap(),
        core_nodes_the_runtime_carries
    );
    let import = imports
        .cell
        .repository
        .import(imports.arm.project())
        .unwrap();
    assert!(
        imports
            .cell
            .repository
            .files_edited_in_the_import(&import)
            .unwrap()
            .is_empty()
    );
}
