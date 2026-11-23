// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Where an import lands, the paths and files it refuses, and what a refused
//! import, delete or create leaves behind.

use super::*;

#[test]
fn a_file_already_in_the_project_is_imported_in_place() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    std::fs::create_dir_all(fixture.project_directory.join("media")).unwrap();
    let in_place = fixture.project_directory.join("media/dropped.wav");
    std::fs::write(&in_place, b"wav").unwrap();

    let applied = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&in_place, "media/dropped.wav")],
        )
        .unwrap()
        .into_applied_transaction();
    assert_eq!(
        repository
            .asset(imported_asset(&applied))
            .unwrap()
            .content_byte_len,
        3
    );
    assert_eq!(std::fs::read(&in_place).unwrap(), b"wav");
}

fn import_into_its_kinds_folder(source: &Path) -> ProjectAssetAction {
    ProjectAssetAction::ImportAsset {
        source: ProjectAssetImportSource::FileOnThisMachine(source.to_path_buf()),
        asset_path: None,
        file_name: None,
        kind: None,
        import_settings: Map::new(),
    }
}

fn the_paths_imported_by(
    applied: &streamlib_project_repository::AppliedProjectTransaction,
) -> Vec<String> {
    applied
        .transaction
        .operations
        .iter()
        .filter_map(|operation| match operation {
            ProjectOperation::Asset(ProjectAssetOperation::ImportAsset { asset }) => {
                Some(asset.path.to_string())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn an_import_with_no_path_is_numbered_past_a_name_an_asset_or_another_file_takes() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let take = fixture.import_source("take.wav", b"take");
    let readme = fixture.import_source("README", b"notes");
    std::fs::create_dir_all(fixture.project_directory.join("media")).unwrap();
    std::fs::write(
        fixture.project_directory.join("media/take.wav"),
        b"untracked",
    )
    .unwrap();

    let applied = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![
                import_into_its_kinds_folder(&take),
                import_into_its_kinds_folder(&take),
                import_into_its_kinds_folder(&readme),
                import_into_its_kinds_folder(&readme),
            ],
        )
        .unwrap()
        .into_applied_transaction();

    assert_eq!(
        the_paths_imported_by(&applied),
        [
            "media/take-2.wav",
            "media/take-3.wav",
            "data/README",
            "data/README-2"
        ]
    );
    assert_eq!(
        std::fs::read(fixture.project_directory.join("media/take.wav")).unwrap(),
        b"untracked"
    );
    assert_eq!(
        std::fs::read(fixture.project_directory.join("media/take-3.wav")).unwrap(),
        b"take"
    );
}

#[test]
fn a_kind_named_by_the_caller_picks_the_folder_an_import_with_no_path_lands_in() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let weights = fixture.import_source("weights.png", b"not an image");
    let applied = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(weights),
                asset_path: None,
                file_name: Some("weights.bin".to_owned()),
                kind: Some(ProjectAssetKind::Data),
                import_settings: Map::new(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    assert_eq!(the_paths_imported_by(&applied), ["data/weights.bin"]);
}

#[test]
fn a_path_another_asset_or_another_file_holds_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let first = fixture.import_source("a.png", b"a");
    let second = fixture.import_source("b.png", b"b");
    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&first, "images/logo.png")])
        .unwrap()
        .into_applied_transaction();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&second, "images/logo.png")]
        ),
        Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset { .. })
    ));
    std::fs::write(fixture.project_directory.join("images/untracked.png"), b"x").unwrap();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&second, "images/untracked.png")]
        ),
        Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile { .. })
    ));
}

#[test]
fn an_upload_is_moved_to_its_assets_path_by_its_hash_and_leaves_the_uploads() {
    use sha2::Digest as _;
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let content_sha256: String = sha2::Sha256::digest(b"uploaded weights")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let upload = the_upload_file_of(&fixture.project_directory, &content_sha256);
    std::fs::create_dir_all(upload.parent().unwrap()).unwrap();
    std::fs::write(&upload, b"uploaded weights").unwrap();
    let import_the_upload =
        |content_sha256: &str, asset_path: &str| ProjectAssetAction::ImportAsset {
            source: ProjectAssetImportSource::UploadedContent(content_sha256.to_owned()),
            asset_path: Some(asset_path.to_owned()),
            file_name: None,
            kind: Some(ProjectAssetKind::Data),
            import_settings: Map::new(),
        };

    let applied = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![import_the_upload(
                &content_sha256,
                "models/weights.safetensors",
            )],
        )
        .unwrap()
        .into_applied_transaction();

    assert_eq!(
        repository
            .asset(imported_asset(&applied))
            .unwrap()
            .content_sha256,
        content_sha256
    );
    assert_eq!(
        std::fs::read(fixture.project_directory.join("models/weights.safetensors")).unwrap(),
        b"uploaded weights"
    );
    assert!(!upload.exists(), "the import empties the uploads");
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_the_upload(&"0".repeat(64), "models/absent.bin")],
        ),
        Err(ProjectRepositoryRefusal::NoSuchUpload(_))
    ));
    std::fs::write(&upload, b"other bytes").unwrap();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_the_upload(&content_sha256, "models/other.bin")],
        ),
        Err(ProjectRepositoryRefusal::UploadHoldsOtherBytes { .. })
    ));
    assert!(
        upload.exists(),
        "a refused import leaves the upload where it landed"
    );
}

#[test]
fn an_import_copies_the_file_once_to_its_path_and_nothing_into_the_project_beside_it() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"video");

    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&video, "media/intro.mp4")])
        .unwrap()
        .into_applied_transaction();

    assert!(!fixture.project_directory.join(".tatolab/content").exists());
    let uploads = fixture.project_directory.join(PROJECT_UPLOADS_DIRECTORY);
    assert!(
        !uploads.exists() || std::fs::read_dir(&uploads).unwrap().next().is_none(),
        "the copy on its way is moved into place"
    );
    assert_eq!(
        std::fs::read(fixture.project_directory.join("media/intro.mp4")).unwrap(),
        b"video"
    );
}

#[test]
fn a_refused_import_leaves_no_copy_in_the_project() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"video");

    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![
                import_from(&video, "media/intro.mp4"),
                ProjectAssetAction::DeleteAsset {
                    asset: Uuid::new_v4(),
                },
            ],
        )
        .unwrap_err();

    assert!(!fixture.project_directory.join("media/intro.mp4").exists());
    let uploads = fixture.project_directory.join(PROJECT_UPLOADS_DIRECTORY);
    assert!(!uploads.exists() || std::fs::read_dir(&uploads).unwrap().next().is_none());
}

#[test]
fn a_stream_asset_is_never_imported_and_a_project_is_never_created_twice() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let graph = fixture.import_source("main.graph.json", b"{}");
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(graph),
                asset_path: Some("streams/main.graph.json".to_owned()),
                file_name: None,
                kind: Some(ProjectAssetKind::Stream),
                import_settings: Map::new(),
            }],
        ),
        Err(ProjectRepositoryRefusal::StreamAssetsAreNotImported)
    ));
    assert!(matches!(
        ProjectRepository::create(&fixture.project_directory, "again", &agent()),
        Err(ProjectRepositoryRefusal::AlreadyAProject { .. })
    ));
    assert!(matches!(
        ProjectRepository::open(Path::new("relative/project")),
        Err(ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(_))
    ));
}

#[test]
fn a_path_differing_only_in_case_from_another_assets_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let first = fixture.import_source("a.png", b"a");
    let second = fixture.import_source("b.png", b"b");
    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&first, "images/Logo.png")])
        .unwrap()
        .into_applied_transaction();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&second, "images/logo.png")]
        ),
        Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset { .. })
    ));
}

#[test]
fn a_graph_document_path_is_never_an_imported_or_relocated_asset() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let data = fixture.import_source("table.json", b"{}");
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(data.clone()),
                asset_path: Some("data/table.graph.json".to_owned()),
                file_name: None,
                kind: Some(ProjectAssetKind::Data),
                import_settings: Map::new(),
            }],
        ),
        Err(ProjectRepositoryRefusal::AssetPathRefused { .. })
    ));
    let asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&data, "data/table.json")])
            .unwrap()
            .into_applied_transaction(),
    );
    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset,
                to_path: "streams/table.json".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset,
                to_path: "streams/main.graph.json".to_owned(),
            }],
        ),
        Err(ProjectRepositoryRefusal::AssetPathRefused { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_path_through_a_symbolic_link_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    std::os::unix::fs::symlink(
        &fixture.import_sources_directory,
        fixture.project_directory.join("linked"),
    )
    .unwrap();
    let image = fixture.import_source("logo.png", b"png");
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&image, "linked/logo.png")]
        ),
        Err(ProjectRepositoryRefusal::AssetPathRefused { .. })
    ));
}

#[test]
fn an_unreadable_graph_document_refuses_every_delete() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let image = fixture.import_source("logo.png", b"png");
    let image_asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&image, "images/logo.png")])
            .unwrap()
            .into_applied_transaction(),
    );
    std::fs::create_dir_all(fixture.project_directory.join("streams")).unwrap();
    std::fs::write(
        fixture.project_directory.join("streams/main.graph.json"),
        b"{ half written",
    )
    .unwrap();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: image_asset }],
        ),
        Err(ProjectRepositoryRefusal::GraphDocumentUnreadable { .. })
    ));
    assert!(fixture.project_directory.join("images/logo.png").exists());
}

#[test]
fn a_create_that_fails_leaves_no_project_behind() {
    let fixture = ProjectBesideItsImportSources::new();
    std::fs::create_dir_all(&fixture.project_directory).unwrap();
    std::fs::write(fixture.project_directory.join(".tatolab"), b"in the way").unwrap();
    assert!(ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).is_err());
    assert!(!fixture.project_directory.join(PROJECT_FILE_NAME).exists());
    assert!(fixture.project_directory.join(".tatolab").is_file());
}

#[cfg(unix)]
#[test]
fn an_in_place_import_of_a_file_that_is_another_assets_file_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let image = fixture.import_source("logo.png", b"png");
    let holder = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&image, "images/logo.png")])
            .unwrap()
            .into_applied_transaction(),
    );
    let alias = fixture.project_directory.join("images/alias.png");
    std::os::unix::fs::symlink(fixture.project_directory.join("images/logo.png"), &alias).unwrap();
    match repository.apply_asset_changes(
        &agent(),
        None,
        vec![import_from(&alias, "images/alias.png")],
    ) {
        Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
            holder: refused_for,
            ..
        }) => {
            assert_eq!(refused_for, holder)
        }
        other => panic!("expected the alias to be refused, got {other:?}"),
    }
    let hard_link = fixture.project_directory.join("images/hard-link.png");
    std::fs::hard_link(
        fixture.project_directory.join("images/logo.png"),
        &hard_link,
    )
    .unwrap();
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![import_from(&hard_link, "images/hard-link.png")]
        ),
        Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset { .. })
    ));
    let reopened = ProjectRepository::open(&fixture.project_directory).unwrap();
    assert_eq!(reopened.assets().unwrap().len(), 1);
    assert_eq!(
        reopened.asset(holder).unwrap().path.to_string(),
        "images/logo.png"
    );
}

#[test]
fn an_asset_a_graph_document_references_cannot_be_deleted() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let image = fixture.import_source("logo.png", b"png");
    let image_asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&image, "images/logo.png")])
            .unwrap()
            .into_applied_transaction(),
    );
    std::fs::create_dir_all(fixture.project_directory.join("streams")).unwrap();
    std::fs::write(
        fixture.project_directory.join("streams/main.graph.json"),
        serde_json::to_vec(
            &json!({"nodes": [{"input_values": {"overlay": {"asset": image_asset.to_string()}}}]}),
        )
        .unwrap(),
    )
    .unwrap();

    let refusal = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: image_asset }],
        )
        .unwrap_err();
    match refusal {
        ProjectRepositoryRefusal::AssetStillReferenced {
            referencing_documents,
            ..
        } => assert_eq!(referencing_documents, vec!["streams/main.graph.json"]),
        other => panic!("expected the reference to refuse the delete, got {other}"),
    }
    assert!(repository.asset(image_asset).is_ok());
}
