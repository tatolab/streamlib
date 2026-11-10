// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, json};
use streamlib_project_repository::{
    PROJECT_FILE_NAME, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    PROJECT_UPLOADS_DIRECTORY, ProjectAssetAction, ProjectAssetImportSource, ProjectAssetKind,
    ProjectAssetOperation, ProjectOperation, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, the_file_of_an_asset, the_upload_file_of,
};
use uuid::Uuid;

struct ProjectBesideItsImportSources {
    _root: tempfile::TempDir,
    project_directory: PathBuf,
    import_sources_directory: PathBuf,
}

impl ProjectBesideItsImportSources {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("robot-cell");
        let import_sources_directory = root.path().join("downloads");
        std::fs::create_dir_all(&import_sources_directory).unwrap();
        Self {
            _root: root,
            project_directory,
            import_sources_directory,
        }
    }

    fn import_source(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.import_sources_directory.join(file_name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

fn agent() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Agent,
        name: "rig agent".to_owned(),
    }
}

fn import_from(source: &Path, asset_path: &str) -> ProjectAssetAction {
    ProjectAssetAction::ImportAsset {
        source: ProjectAssetImportSource::FileOnThisMachine(source.to_path_buf()),
        asset_path: Some(asset_path.to_owned()),
        file_name: None,
        kind: None,
        import_settings: Map::new(),
    }
}

fn imported_asset(applied: &streamlib_project_repository::AppliedProjectTransaction) -> Uuid {
    match &applied.transaction.operations[0] {
        ProjectOperation::Asset(ProjectAssetOperation::ImportAsset { asset }) => asset.asset,
        other => panic!("expected an import, got {other:?}"),
    }
}

fn transactions_in_history(repository: &ProjectRepository) -> Vec<ProjectTransaction> {
    repository
        .history()
        .commits_newest_first()
        .unwrap()
        .into_iter()
        .filter_map(|commit| {
            commit
                .message
                .lines()
                .find_map(|line| {
                    line.strip_prefix(&format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: "))
                })
                .map(|json| serde_json::from_str(json).unwrap())
        })
        .collect()
}

#[test]
fn a_media_file_a_usd_scene_and_an_fmu_are_imported_renamed_moved_and_deleted_one_commit_each() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();

    let video = fixture.import_source("intro.mp4", b"not really h264");
    let scene = fixture.import_source("arm.usda", b"#usda 1.0\n");
    let fmu = fixture.import_source("arm.fmu", b"PK fmu");

    let video_asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&video, "media/intro.mp4")])
            .unwrap()
            .into_applied_transaction(),
    );
    let scene_asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&scene, "models/arm.usda")])
            .unwrap()
            .into_applied_transaction(),
    );
    let fmu_asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&fmu, "data/arm.fmu")])
            .unwrap()
            .into_applied_transaction(),
    );
    assert_eq!(
        repository.asset(video_asset).unwrap().kind,
        ProjectAssetKind::Media
    );
    assert_eq!(
        repository.asset(scene_asset).unwrap().kind,
        ProjectAssetKind::Scene
    );
    assert_eq!(
        repository.asset(fmu_asset).unwrap().kind,
        ProjectAssetKind::Data
    );
    assert_eq!(
        std::fs::read(fixture.project_directory.join("media/intro.mp4")).unwrap(),
        b"not really h264"
    );

    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: video_asset,
                to_path: "media/opening.mp4".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: video_asset,
                to_path: "archive/2026/opening.mp4".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: fmu_asset }],
        )
        .unwrap()
        .into_applied_transaction();

    let moved = repository.asset(video_asset).unwrap();
    assert_eq!(moved.path.as_str(), "archive/2026/opening.mp4");
    assert!(
        fixture
            .project_directory
            .join("archive/2026/opening.mp4")
            .exists()
    );
    assert!(
        fixture
            .project_directory
            .join("archive/2026/opening.mp4.asset.json")
            .exists()
    );
    assert!(!fixture.project_directory.join("media/intro.mp4").exists());
    assert!(
        !fixture
            .project_directory
            .join("media/intro.mp4.asset.json")
            .exists()
    );
    assert!(!fixture.project_directory.join("data/arm.fmu").exists());
    assert!(matches!(
        repository.asset(fmu_asset),
        Err(ProjectRepositoryRefusal::NoSuchAsset(_))
    ));

    let commits = repository.history().commits_newest_first().unwrap();
    assert_eq!(commits.len(), 7, "create plus six transactions");
    assert_eq!(transactions_in_history(&repository).len(), 6);

    let reopened = ProjectRepository::open(&fixture.project_directory).unwrap();
    assert_eq!(
        reopened
            .assets()
            .unwrap()
            .into_iter()
            .map(|asset| asset.asset)
            .collect::<Vec<_>>(),
        {
            let mut expected = vec![video_asset, scene_asset];
            expected.sort();
            expected
        }
    );
    assert_eq!(
        reopened.asset(video_asset).unwrap().path.as_str(),
        "archive/2026/opening.mp4"
    );
}

#[test]
fn stock_git_clones_the_history_and_finds_sidecars_but_never_the_content() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"BYTES THAT MUST NOT ENTER HISTORY");
    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&video, "media/intro.mp4")])
        .unwrap()
        .into_applied_transaction();

    let clone_directory = tempfile::tempdir().unwrap();
    let clone = clone_directory.path().join("clone");
    let cloned = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(fixture.project_directory.join(PROJECT_HISTORY_DIRECTORY))
        .arg(&clone)
        .output()
        .expect("git runs");
    assert!(
        cloned.status.success(),
        "{}",
        String::from_utf8_lossy(&cloned.stderr)
    );

    let mut cloned_files: Vec<String> = Vec::new();
    let mut directories = vec![clone.clone()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                directories.push(entry.path());
            } else {
                cloned_files.push(
                    entry
                        .path()
                        .strip_prefix(&clone)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    cloned_files.sort();
    assert_eq!(
        cloned_files,
        vec!["media/intro.mp4.asset.json", PROJECT_FILE_NAME]
    );

    let log = Command::new("git")
        .args(["log", "--format=%an|%s"])
        .current_dir(&clone)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(log.stdout).unwrap(),
        "rig agent|import_asset media/intro.mp4\nrig agent|create_project robot-cell\n"
    );
    let every_object = Command::new("git")
        .args([
            "grep",
            "--quiet",
            "BYTES THAT MUST NOT ENTER HISTORY",
            "HEAD",
        ])
        .current_dir(&clone)
        .status()
        .unwrap();
    assert!(!every_object.success(), "the content reached history");
}

#[test]
fn a_transaction_with_one_refused_change_changes_nothing_and_commits_nothing() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"video");
    let head_before = repository.history().head_commit().unwrap();

    let refusal = repository
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
    assert!(
        matches!(refusal, ProjectRepositoryRefusal::NoSuchAsset(_)),
        "{refusal}"
    );
    assert_eq!(repository.history().head_commit().unwrap(), head_before);
    assert_eq!(repository.assets().unwrap().len(), 0);
    assert!(!fixture.project_directory.join("media/intro.mp4").exists());
    assert!(
        !fixture
            .project_directory
            .join("media/intro.mp4.asset.json")
            .exists()
    );
}

#[test]
fn a_transaction_records_its_author_parent_and_inverse() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"video");
    let parent = repository.history().head_commit().unwrap();
    let applied = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![
                import_from(&video, "media/intro.mp4"),
                ProjectAssetAction::SetAssetSettings {
                    asset: Uuid::nil(),
                    import_settings: Map::new(),
                },
            ],
        )
        .unwrap_err();
    assert!(matches!(applied, ProjectRepositoryRefusal::NoSuchAsset(_)));

    let applied = repository
        .apply_asset_changes(&agent(), None, vec![import_from(&video, "media/intro.mp4")])
        .unwrap()
        .into_applied_transaction();
    let video_asset = imported_asset(&applied);
    let settings = json!({"color_space": "rec709"})
        .as_object()
        .unwrap()
        .clone();
    let set = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::SetAssetSettings {
                asset: video_asset,
                import_settings: settings.clone(),
            }],
        )
        .unwrap()
        .into_applied_transaction();

    let recorded = transactions_in_history(&repository);
    assert_eq!(recorded[0], set.transaction);
    assert_eq!(recorded[1], applied.transaction);
    assert_eq!(applied.transaction.parent_commit, parent);
    assert_eq!(set.transaction.parent_commit, Some(applied.commit.clone()));
    assert_eq!(set.transaction.author, agent());
    assert_eq!(
        set.transaction.inverse_operations,
        vec![set.transaction.operations[0].inverse_at(&applied.commit)]
    );
    assert_eq!(
        repository.asset(video_asset).unwrap().import_settings,
        settings
    );
    let sidecar: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.project_directory.join("media/intro.mp4.asset.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["import_settings"]["color_space"], "rec709");
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
            &json!({"nodes": [{"config": {"overlay": {"asset": image_asset.to_string()}}}]}),
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
fn a_file_replaced_at_its_path_is_what_its_asset_reads_and_history_grows_by_no_bytes() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let video = fixture.import_source("intro.mp4", b"take one");
    let applied = repository
        .apply_asset_changes(&agent(), None, vec![import_from(&video, "media/intro.mp4")])
        .unwrap()
        .into_applied_transaction();
    let asset = imported_asset(&applied);
    let file = fixture.project_directory.join("media/intro.mp4");
    let large_take = vec![7u8; 4 << 20];

    std::fs::write(&file, &large_take).unwrap();
    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset,
                to_path: "media/intro-take-two.mp4".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();

    let renamed = the_file_of_an_asset(&fixture.project_directory, asset).unwrap();
    assert_eq!(std::fs::read(&renamed).unwrap(), large_take);
    let files_at_head = repository.history().files_at_head().unwrap();
    assert!(files_at_head.contains_key("media/intro-take-two.mp4.asset.json"));
    assert!(!files_at_head.contains_key("media/intro-take-two.mp4"));
    assert!(files_at_head.values().all(|bytes| bytes.len() < 4 << 10));
}

#[test]
fn an_undone_delete_brings_back_the_sidecar_and_the_asset_reads_missing_until_its_file_returns() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let lut = fixture.import_source("grade.cube", b"LUT_3D_SIZE 2");
    let asset = imported_asset(
        &repository
            .apply_asset_changes(&agent(), None, vec![import_from(&lut, "looks/grade.cube")])
            .unwrap()
            .into_applied_transaction(),
    );
    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset { asset }],
        )
        .unwrap()
        .into_applied_transaction();
    let file = fixture.project_directory.join("looks/grade.cube");
    assert!(!file.exists());

    let reversal = repository.undo_by(&agent()).unwrap();
    repository
        .apply_project_operations(
            &agent(),
            None,
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        )
        .unwrap()
        .into_applied_transaction();

    assert_eq!(
        repository.asset(asset).unwrap().path.as_str(),
        "looks/grade.cube"
    );
    assert!(matches!(
        the_file_of_an_asset(&fixture.project_directory, asset),
        Err(ProjectRepositoryRefusal::AssetFileMissing { file: missing, .. }) if missing == file
    ));
    std::fs::write(&file, b"LUT_3D_SIZE 2").unwrap();
    assert_eq!(
        the_file_of_an_asset(&fixture.project_directory, asset).unwrap(),
        file
    );
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

#[test]
fn an_asset_whose_file_ends_like_a_graph_document_never_enters_history() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let data = fixture.import_source("table.json", b"{\"secret\": 1}");
    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&data, "data/table.json")])
        .unwrap()
        .into_applied_transaction();
    drop(repository);
    let data_directory = fixture.project_directory.join("data");
    std::fs::rename(
        data_directory.join("table.json"),
        data_directory.join("table.graph.json"),
    )
    .unwrap();
    std::fs::rename(
        data_directory.join("table.json.asset.json"),
        data_directory.join("table.graph.json.asset.json"),
    )
    .unwrap();

    let mut repository = ProjectRepository::open(&fixture.project_directory).unwrap();
    let image = fixture.import_source("logo.png", b"png");
    repository
        .apply_asset_changes(&agent(), None, vec![import_from(&image, "images/logo.png")])
        .unwrap()
        .into_applied_transaction();
    let files_at_head = repository.history().files_at_head().unwrap();
    assert!(files_at_head.contains_key("data/table.graph.json.asset.json"));
    assert!(!files_at_head.contains_key("data/table.graph.json"));
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

fn write_stream_document(project_directory: &Path, path: &str, document: &serde_json::Value) {
    let on_disk = project_directory.join(path);
    std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
    std::fs::write(on_disk, serde_json::to_vec_pretty(document).unwrap()).unwrap();
}

#[test]
fn a_stream_is_an_asset_its_graph_document_names_and_it_moves_and_deletes_as_one_file() {
    let fixture = ProjectBesideItsImportSources::new();
    let stream_asset = Uuid::new_v4();
    write_stream_document(
        &fixture.project_directory,
        "streams/arm-control.graph.json",
        &json!({"asset": stream_asset.to_string(), "nodes": [], "links": []}),
    );
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    assert!(
        repository
            .history()
            .files_at_head()
            .unwrap()
            .contains_key("streams/arm-control.graph.json")
    );

    let listed = repository.asset(stream_asset).unwrap();
    assert_eq!(listed.kind, ProjectAssetKind::Stream);
    assert_eq!(listed.path.as_str(), "streams/arm-control.graph.json");

    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: stream_asset,
                to_path: "streams/arm-control.json".to_owned(),
            }],
        ),
        Err(ProjectRepositoryRefusal::AssetPathRefused { .. })
    ));
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(), None,
            vec![ProjectAssetAction::SetAssetSettings {
                asset: stream_asset,
                import_settings: Map::new(),
            }],
        ),
        Err(ProjectRepositoryRefusal::StreamAssetHasNoImportSettings(refused)) if refused == stream_asset
    ));

    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: stream_asset,
                to_path: "streams/cell/arm-control.graph.json".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    let moved = repository.asset(stream_asset).unwrap();
    assert_eq!(moved.path.as_str(), "streams/cell/arm-control.graph.json");
    let files_at_head = repository.history().files_at_head().unwrap();
    assert!(files_at_head.contains_key("streams/cell/arm-control.graph.json"));
    assert!(!files_at_head.contains_key("streams/arm-control.graph.json"));
    assert!(
        !files_at_head
            .keys()
            .any(|path| path.ends_with(".graph.json.asset.json"))
    );

    write_stream_document(
        &fixture.project_directory,
        "streams/reader.graph.json",
        &json!({
            "asset": Uuid::new_v4().to_string(),
            "nodes": [{"name": "player", "config": {"stream": {"asset": stream_asset.to_string()}}}],
        }),
    );
    match repository.apply_asset_changes(
        &agent(),
        None,
        vec![ProjectAssetAction::DeleteAsset {
            asset: stream_asset,
        }],
    ) {
        Err(ProjectRepositoryRefusal::AssetStillReferenced {
            referencing_documents,
            ..
        }) => assert_eq!(referencing_documents, ["streams/reader.graph.json"]),
        other => panic!("expected the referenced stream to be kept, got {other:?}"),
    }
    std::fs::remove_file(fixture.project_directory.join("streams/reader.graph.json")).unwrap();

    let deleted = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset {
                asset: stream_asset,
            }],
        )
        .unwrap()
        .into_applied_transaction();
    assert!(
        !fixture
            .project_directory
            .join("streams/cell/arm-control.graph.json")
            .exists()
    );
    assert!(matches!(
        repository.asset(stream_asset),
        Err(ProjectRepositoryRefusal::NoSuchAsset(_))
    ));
    match &deleted.transaction.inverse_operations[..] {
        [
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument {
                    stream, document, ..
                },
            ),
        ] => {
            assert_eq!(*stream, stream_asset);
            assert_eq!(document["asset"], stream_asset.to_string());
        }
        other => panic!("expected the delete's inverse to write its document back, got {other:?}"),
    }
}

#[test]
fn a_stream_document_whose_asset_cannot_be_read_is_still_one_stream_asset() {
    let fixture = ProjectBesideItsImportSources::new();
    write_stream_document(
        &fixture.project_directory,
        "streams/main.graph.json",
        &json!({"nodes": []}),
    );
    let repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let assets = repository.assets().unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, ProjectAssetKind::Stream);
    assert_eq!(
        ProjectRepository::open(&fixture.project_directory)
            .unwrap()
            .assets()
            .unwrap()[0]
            .asset,
        assets[0].asset
    );
}

#[test]
fn a_project_whose_files_are_already_written_is_committed_whole_as_its_first_commit_once() {
    let fixture = ProjectBesideItsImportSources::new();
    let project_file = streamlib_project_repository::TatolabProjectFile::of_a_new_project("rig");
    std::fs::create_dir_all(&fixture.project_directory).unwrap();
    std::fs::write(
        fixture.project_directory.join(PROJECT_FILE_NAME),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();
    let stream_asset = Uuid::new_v4();
    write_stream_document(
        &fixture.project_directory,
        "streams/main.graph.json",
        &json!({"asset": stream_asset.to_string(), "nodes": [], "links": []}),
    );

    let repository = ProjectRepository::commit_a_written_project_as_its_first(
        &fixture.project_directory,
        &agent(),
    )
    .unwrap();

    let files_at_head = repository.history().files_at_head().unwrap();
    assert_eq!(
        files_at_head.keys().collect::<Vec<_>>(),
        vec!["streams/main.graph.json", PROJECT_FILE_NAME]
    );
    assert_eq!(repository.project_file(), &project_file);
    assert_eq!(
        repository.history().commits_newest_first().unwrap()[0].message,
        "create_project rig"
    );
    assert!(matches!(
        ProjectRepository::commit_a_written_project_as_its_first(
            &fixture.project_directory,
            &agent(),
        ),
        Err(ProjectRepositoryRefusal::ProjectAlreadyCommitted { .. })
    ));
}

#[test]
fn the_history_records_the_whole_project_but_what_is_generated_hidden_or_an_assets_bytes() {
    let fixture = ProjectBesideItsImportSources::new();
    let project = &fixture.project_directory;
    let project_file = streamlib_project_repository::TatolabProjectFile::of_a_new_project("rig");
    for (path, bytes) in [
        ("nodes/halftone.py", &b"class Halftone: ...\n"[..]),
        ("pyproject.toml", b"[project]\nname = \"rig\"\n"),
        (".python-version", b"3.12\n"),
        (".gitignore", b".venv/\n"),
        (".venv/pyvenv.cfg", b"home = /usr\n"),
        ("venv/pyvenv.cfg", b"home = /usr\n"),
        ("venv/lib/site.py", b""),
        (".git/HEAD", b"ref: refs/heads/main\n"),
        ("node_modules/left-pad/index.js", b""),
        ("nodes/__pycache__/halftone.cpython-312.pyc", b"\0"),
        ("media/dropped.wav", b"wav"),
    ] {
        let on_disk = project.join(path);
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(on_disk, bytes).unwrap();
    }
    std::os::unix::fs::symlink(
        fixture.import_source("elsewhere.py", b""),
        project.join("nodes/linked.py"),
    )
    .unwrap();
    std::fs::write(
        project.join(PROJECT_FILE_NAME),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();
    write_stream_document(
        project,
        "streams/main.graph.json",
        &json!({"asset": Uuid::new_v4().to_string(), "nodes": [], "links": []}),
    );
    let mut repository =
        ProjectRepository::commit_a_written_project_as_its_first(project, &agent()).unwrap();

    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![import_from(
                &project.join("media/dropped.wav"),
                "media/dropped.wav",
            )],
        )
        .unwrap()
        .into_applied_transaction();

    let files_at_head = repository.history().files_at_head().unwrap();
    assert_eq!(
        files_at_head.keys().collect::<Vec<_>>(),
        vec![
            ".gitignore",
            ".python-version",
            "media/dropped.wav.asset.json",
            "nodes/halftone.py",
            "pyproject.toml",
            "streams/main.graph.json",
            PROJECT_FILE_NAME,
        ]
    );
    assert_eq!(files_at_head["nodes/halftone.py"], b"class Halftone: ...\n");
}

#[test]
fn an_assets_file_is_the_file_at_its_path_and_an_unknown_or_missing_asset_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let lut = fixture.import_source("grade.cube", b"LUT_3D_SIZE 2");
    let applied = repository
        .apply_asset_changes(&agent(), None, vec![import_from(&lut, "looks/grade.cube")])
        .unwrap()
        .into_applied_transaction();
    let asset = imported_asset(&applied);

    let file = the_file_of_an_asset(&fixture.project_directory, asset).unwrap();
    assert_eq!(file, fixture.project_directory.join("looks/grade.cube"));
    assert_eq!(std::fs::read(&file).unwrap(), b"LUT_3D_SIZE 2");

    let unknown_asset = Uuid::new_v4();
    assert!(matches!(
        the_file_of_an_asset(&fixture.project_directory, unknown_asset),
        Err(ProjectRepositoryRefusal::NoSuchAsset(refused)) if refused == unknown_asset
    ));

    std::fs::remove_file(&file).unwrap();
    assert!(matches!(
        the_file_of_an_asset(&fixture.project_directory, asset),
        Err(ProjectRepositoryRefusal::AssetFileMissing { .. })
    ));
}
