// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An asset imported, renamed, moved, replaced, deleted and brought back one
//! commit at a time, the transaction that records it, and the history a clone
//! of the project reads.

use super::*;

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
