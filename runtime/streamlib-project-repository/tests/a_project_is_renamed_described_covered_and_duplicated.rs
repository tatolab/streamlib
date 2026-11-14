// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use serde_json::Map;
use streamlib_project_repository::{
    PROJECT_HISTORY_DIRECTORY, ProjectAssetAction, ProjectAssetImportSource, ProjectAssetOperation,
    ProjectOperation, ProjectRepository, ProjectRepositoryRefusal, ProjectSettingsAction,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, TatolabProjectFile,
};
use uuid::Uuid;

struct ProjectInATemporaryRoot {
    root: tempfile::TempDir,
    project_directory: PathBuf,
}

impl ProjectInATemporaryRoot {
    fn created() -> (Self, ProjectRepository) {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("robot-cell");
        let repository =
            ProjectRepository::create(&project_directory, "Robot cell", &person()).unwrap();
        (
            Self {
                root,
                project_directory,
            },
            repository,
        )
    }

    fn beside_the_project(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
}

fn person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jonathan".to_owned(),
    }
}

fn apply_settings(
    repository: &mut ProjectRepository,
    settings_action: ProjectSettingsAction,
) -> Result<(), ProjectRepositoryRefusal> {
    repository
        .apply_project_settings_action(&person(), None, settings_action)
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

fn project_file_on_disk(project_directory: &Path) -> TatolabProjectFile {
    TatolabProjectFile::read_from_project_directory(project_directory).unwrap()
}

fn import_an_image(repository: &mut ProjectRepository, source_directory: &Path) -> Uuid {
    let source = source_directory.join("cover.png");
    std::fs::write(&source, b"\x89PNG not really").unwrap();
    let applied = repository
        .apply_asset_changes(
            &person(),
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(source),
                asset_path: Some("assets/cover.png".to_owned()),
                file_name: None,
                kind: None,
                import_settings: Map::new(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    match &applied.transaction.operations[0] {
        ProjectOperation::Asset(ProjectAssetOperation::ImportAsset { asset }) => asset.asset,
        other => panic!("expected an import, got {other:?}"),
    }
}

#[test]
fn a_rename_writes_the_project_file_keeps_its_uuid_and_undoes() {
    let (project, mut repository) = ProjectInATemporaryRoot::created();
    let uuid = repository.project_file().project;
    apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectName {
            name: "Weld line".to_owned(),
        },
    )
    .unwrap();
    let on_disk = project_file_on_disk(&project.project_directory);
    assert_eq!(on_disk.name, "Weld line");
    assert_eq!(on_disk.project, uuid);
    assert!(
        repository.history().commits_newest_first().unwrap()[0]
            .message
            .starts_with("rename_project Weld line")
    );

    undo(&mut repository);
    assert_eq!(
        project_file_on_disk(&project.project_directory).name,
        "Robot cell"
    );
    assert_eq!(repository.project_file().name, "Robot cell");
}

#[test]
fn a_blank_name_is_refused_with_nothing_committed() {
    let (_project, mut repository) = ProjectInATemporaryRoot::created();
    let commits_before = repository.history().commits_newest_first().unwrap().len();
    let refusal = apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectName {
            name: "  ".to_owned(),
        },
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        ProjectRepositoryRefusal::BlankProjectName
    ));
    assert_eq!(
        repository.history().commits_newest_first().unwrap().len(),
        commits_before
    );
}

#[test]
fn a_description_and_a_cover_are_set_cleared_and_undone_independently() {
    let (project, mut repository) = ProjectInATemporaryRoot::created();
    let cover = import_an_image(&mut repository, project.root.path());
    apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectDescription {
            description: Some("Two arms and a camera".to_owned()),
        },
    )
    .unwrap();
    apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectCover { cover: Some(cover) },
    )
    .unwrap();
    let on_disk = project_file_on_disk(&project.project_directory);
    assert_eq!(
        on_disk.description.as_deref(),
        Some("Two arms and a camera")
    );
    assert_eq!(on_disk.cover, Some(cover));

    undo(&mut repository);
    let on_disk = project_file_on_disk(&project.project_directory);
    assert_eq!(on_disk.cover, None);
    assert_eq!(
        on_disk.description.as_deref(),
        Some("Two arms and a camera")
    );
}

#[test]
fn a_cover_must_be_a_media_asset_the_project_holds_and_cannot_be_deleted_while_it_is_one() {
    let (project, mut repository) = ProjectInATemporaryRoot::created();
    let no_such_asset = Uuid::new_v4();
    assert!(matches!(
        apply_settings(
            &mut repository,
            ProjectSettingsAction::SetProjectCover {
                cover: Some(no_such_asset),
            },
        ),
        Err(ProjectRepositoryRefusal::NoSuchAsset(refused)) if refused == no_such_asset
    ));

    let cover = import_an_image(&mut repository, project.root.path());
    apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectCover { cover: Some(cover) },
    )
    .unwrap();
    let refusal = repository
        .apply_asset_changes(
            &person(),
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: cover }],
        )
        .unwrap_err();
    assert!(
        matches!(refusal, ProjectRepositoryRefusal::AssetIsTheProjectCover(refused) if refused == cover),
        "{refusal}"
    );
}

#[test]
fn last_edited_is_the_newest_commit_of_history() {
    let (_project, mut repository) = ProjectInATemporaryRoot::created();
    let created_at = repository.last_edited_at_unix_seconds().unwrap().unwrap();
    apply_settings(
        &mut repository,
        ProjectSettingsAction::SetProjectDescription {
            description: Some("a cell".to_owned()),
        },
    )
    .unwrap();
    let edited_at = repository.last_edited_at_unix_seconds().unwrap().unwrap();
    assert!(edited_at >= created_at);
    assert_eq!(
        Some(edited_at),
        repository
            .history()
            .commits_newest_first()
            .unwrap()
            .first()
            .map(|commit| commit.committed_at_unix_seconds)
    );
}

#[test]
fn a_duplicate_carries_the_files_and_history_under_a_new_uuid_and_leaves_the_runtime_folder() {
    let (project, mut repository) = ProjectInATemporaryRoot::created();
    import_an_image(&mut repository, project.root.path());
    std::fs::create_dir_all(project.project_directory.join(".venv")).unwrap();
    std::fs::write(
        project.project_directory.join(".venv/pyvenv.cfg"),
        b"home = /x",
    )
    .unwrap();
    std::fs::create_dir_all(project.project_directory.join(".tatolab/logs")).unwrap();
    std::fs::write(
        project
            .project_directory
            .join(".tatolab/logs/project.jsonl"),
        b"{}\n",
    )
    .unwrap();
    let commits_of_the_original = repository.history().commits_newest_first().unwrap().len();

    let into = project.beside_the_project("robot-cell-copy");
    let duplicate = repository.duplicate_into(&into, &person()).unwrap();

    let duplicate_file = project_file_on_disk(&into);
    assert_ne!(duplicate_file.project, repository.project_file().project);
    assert_eq!(duplicate_file.name, "Robot cell copy");
    assert!(into.join("assets/cover.png").is_file());
    assert!(into.join("assets/cover.png.asset.json").is_file());
    assert!(into.join(PROJECT_HISTORY_DIRECTORY).is_dir());
    assert!(!into.join(".venv").exists());
    assert!(!into.join(".tatolab/logs").exists());

    let duplicate_commits = duplicate.history().commits_newest_first().unwrap();
    assert_eq!(duplicate_commits.len(), commits_of_the_original + 1);
    assert_eq!(
        duplicate_commits[0].message,
        format!(
            "duplicate_project Robot cell copy from {}",
            repository.project_file().project
        )
    );
    assert_eq!(duplicate.assets().unwrap(), repository.assets().unwrap());
    assert_eq!(
        repository.history().commits_newest_first().unwrap().len(),
        commits_of_the_original
    );
}

#[test]
fn an_edit_of_the_duplicate_leaves_the_original_as_it_was() {
    let (project, repository) = ProjectInATemporaryRoot::created();
    let into = project.beside_the_project("robot-cell-copy");
    let mut duplicate = repository.duplicate_into(&into, &person()).unwrap();
    apply_settings(
        &mut duplicate,
        ProjectSettingsAction::SetProjectName {
            name: "Weld line".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(project_file_on_disk(&into).name, "Weld line");
    assert_eq!(
        project_file_on_disk(&project.project_directory).name,
        "Robot cell"
    );
}

#[test]
fn a_duplicate_into_an_existing_path_or_inside_the_project_is_refused_leaving_nothing() {
    let (project, repository) = ProjectInATemporaryRoot::created();
    let existing = project.beside_the_project("taken");
    std::fs::create_dir_all(&existing).unwrap();
    assert!(matches!(
        repository.duplicate_into(&existing, &person()),
        Err(ProjectRepositoryRefusal::DuplicateIntoAnExistingPath { .. })
    ));
    assert_eq!(std::fs::read_dir(&existing).unwrap().count(), 0);

    let inside = project.project_directory.join("nested/copy");
    assert!(matches!(
        repository.duplicate_into(&inside, &person()),
        Err(ProjectRepositoryRefusal::DuplicateIntoTheProjectItself { .. })
    ));
    assert!(!project.project_directory.join("nested").exists());

    assert!(matches!(
        repository.duplicate_into(Path::new("relative/copy"), &person()),
        Err(ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(_))
    ));
}
