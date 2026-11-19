// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use streamlib_project_repository::{
    ProjectRepository, ProjectRepositoryRefusal, ProjectSettingsAction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, TatolabProjectFile,
};

fn person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jonathan".to_owned(),
    }
}

fn file_under(
    repository: &mut ProjectRepository,
    node_type: &str,
    group: Option<&str>,
) -> Result<(), ProjectRepositoryRefusal> {
    repository
        .apply_project_settings_action(
            &person(),
            None,
            ProjectSettingsAction::SetNodeTypeGroup {
                node_type: node_type.to_owned(),
                group: group.map(str::to_owned),
            },
        )
        .map(|_| ())
}

fn undo(repository: &mut ProjectRepository) {
    let reversal = repository.undo_by(&person()).unwrap();
    repository
        .apply_project_operations(
            &person(),
            None,
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        )
        .unwrap();
}

fn groups_on_disk(project_directory: &Path) -> Vec<(String, String)> {
    TatolabProjectFile::read_from_project_directory(project_directory)
        .unwrap()
        .node_type_groups
        .into_iter()
        .collect()
}

#[test]
fn a_node_type_filed_under_a_group_is_written_to_the_project_file_and_one_undo_takes_it_back() {
    let root = tempfile::tempdir().unwrap();
    let project_directory = root.path().join("robot-cell");
    let mut repository =
        ProjectRepository::create(&project_directory, "Robot cell", &person()).unwrap();

    file_under(
        &mut repository,
        "streamlib::core::Posterize",
        Some("Effects/Stylize"),
    )
    .unwrap();
    assert_eq!(
        groups_on_disk(&project_directory),
        [(
            "streamlib::core::Posterize".to_owned(),
            "Effects/Stylize".to_owned()
        )]
    );
    assert!(
        repository.history().commits_newest_first().unwrap()[0]
            .message
            .starts_with("set_node_type_group streamlib::core::Posterize")
    );

    file_under(&mut repository, "streamlib::core::Posterize", Some("Show")).unwrap();
    undo(&mut repository);
    assert_eq!(
        groups_on_disk(&project_directory),
        [(
            "streamlib::core::Posterize".to_owned(),
            "Effects/Stylize".to_owned()
        )],
        "undo puts back the group it was moved from"
    );

    file_under(&mut repository, "streamlib::core::Posterize", None).unwrap();
    assert!(groups_on_disk(&project_directory).is_empty());
    assert!(
        !std::fs::read_to_string(project_directory.join("tatolab-project.json"))
            .unwrap()
            .contains("node_type_groups"),
        "a project that moved nothing writes no map"
    );
    undo(&mut repository);
    assert_eq!(
        repository.project_file().node_type_groups["streamlib::core::Posterize"],
        "Effects/Stylize"
    );
}

#[test]
fn a_group_that_is_no_path_and_a_blank_node_type_are_refused_with_nothing_committed() {
    let root = tempfile::tempdir().unwrap();
    let project_directory = root.path().join("robot-cell");
    let mut repository =
        ProjectRepository::create(&project_directory, "Robot cell", &person()).unwrap();
    let commits_before = repository.history().commits_newest_first().unwrap().len();

    for group in ["", "Effects//Stylize", "/Effects", "Effects/", "  "] {
        let refusal = file_under(&mut repository, "app:Blur", Some(group)).unwrap_err();
        assert!(
            matches!(
                refusal,
                ProjectRepositoryRefusal::NodeTypeGroupIsNotAPath { .. }
            ),
            "{group:?}: {refusal}"
        );
    }
    assert!(matches!(
        file_under(&mut repository, " ", Some("Effects")).unwrap_err(),
        ProjectRepositoryRefusal::BlankNodeTypeMovedIntoAGroup
    ));
    assert_eq!(
        repository.history().commits_newest_first().unwrap().len(),
        commits_before
    );
    assert!(groups_on_disk(&project_directory).is_empty());
}

#[test]
fn a_project_imported_into_another_carries_its_groups_into_its_imports_folder() {
    let root = tempfile::tempdir().unwrap();
    let mut arm =
        ProjectRepository::create(&root.path().join("robot-arm"), "Robot arm", &person()).unwrap();
    file_under(&mut arm, "arm_nodes:Gripper", Some("Robot/Hands")).unwrap();
    let mut cell =
        ProjectRepository::create(&root.path().join("robot-cell"), "Robot cell", &person())
            .unwrap();

    cell.add_dependency(
        &person(),
        None,
        &arm.project_directory().display().to_string(),
        None,
    )
    .unwrap();

    assert_eq!(
        groups_on_disk(&cell.project_directory().join("imports/robot-arm")),
        [("arm_nodes:Gripper".to_owned(), "Robot/Hands".to_owned())]
    );
    assert!(
        cell.project_file().node_type_groups.is_empty(),
        "the import's map stays its own; the importing project's map is its own to set"
    );
}
