// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's asset bytes travel through the Git LFS endpoint beside its history remote: a push
//! uploads them before the history, and a clone and a fetch bring them back.

mod support;

use std::path::Path;

use streamlib_project_repository::{
    PROJECT_HISTORY_DIRECTORY, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind,
};
use support::{REMOTE_BEARER_TOKEN, SmartHttpRemoteServedByGitHttpBackend};

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jonathan".to_owned(),
    }
}

fn import(project: &mut ProjectRepository, parent: &Path, file_name: &str, bytes: &[u8]) {
    std::fs::write(parent.join(file_name), bytes).unwrap();
    project
        .apply_asset_changes(
            &a_person(),
            vec![ProjectAssetChangeRequest::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(parent.join(file_name)),
                asset_path: format!("media/{file_name}"),
                kind: None,
                import_settings: serde_json::Map::new(),
            }],
        )
        .unwrap();
}

fn the_arm_project(parent: &Path) -> ProjectRepository {
    let mut project = ProjectRepository::create(
        &parent.join("arm-control"),
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    import(&mut project, parent, "intro.mp4", b"video bytes");
    project
}

fn content_sha256_of_the_asset_at(project: &ProjectRepository, asset_path: &str) -> String {
    project
        .assets()
        .unwrap()
        .into_iter()
        .find(|asset| asset.path.to_string() == asset_path)
        .unwrap()
        .content_sha256
}

#[test]
fn a_push_uploads_the_bytes_before_the_history_and_a_clone_arrives_complete() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = the_arm_project(parent.path());
    let intro_sha256 = content_sha256_of_the_asset_at(&arm, "media/intro.mp4");

    arm.push_to_remote(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let requests = remote_server.requests();
    let uploaded_at = requests
        .iter()
        .position(|request| *request == format!("upload {intro_sha256}"))
        .unwrap();
    let history_pushed_at = requests
        .iter()
        .position(|request| request == "git git-receive-pack")
        .unwrap();
    assert!(uploaded_at < history_pushed_at, "{requests:?}");
    assert!(requests.contains(&format!("verify {intro_sha256}")));

    let clone_directory = parent.path().join("elsewhere/arm-control");
    let clone = ProjectRepository::clone_from_remote(
        &clone_directory,
        &remote_server.remote(Some(REMOTE_BEARER_TOKEN)),
    )
    .unwrap();
    assert_eq!(
        clone.history().head_commit().unwrap(),
        arm.history().head_commit().unwrap()
    );
    assert_eq!(clone.project_file(), arm.project_file());
    assert_eq!(clone.assets().unwrap(), arm.assets().unwrap());
    assert_eq!(
        std::fs::read(clone_directory.join("media/intro.mp4")).unwrap(),
        b"video bytes"
    );
    assert_eq!(
        std::fs::read(clone.content_store().content_path(&intro_sha256)).unwrap(),
        b"video bytes"
    );
}

#[test]
fn a_second_push_sends_only_the_bytes_the_remote_lacks() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let mut arm = the_arm_project(parent.path());
    let remote = remote_server.remote(Some(REMOTE_BEARER_TOKEN));
    arm.push_to_remote(&remote).unwrap();
    import(&mut arm, parent.path(), "outro.mp4", b"more video");
    arm.push_to_remote(&remote).unwrap();

    let uploads: Vec<String> = remote_server
        .requests()
        .into_iter()
        .filter(|request| request.starts_with("upload "))
        .collect();
    assert_eq!(
        uploads,
        vec![
            format!(
                "upload {}",
                content_sha256_of_the_asset_at(&arm, "media/intro.mp4")
            ),
            format!(
                "upload {}",
                content_sha256_of_the_asset_at(&arm, "media/outro.mp4")
            ),
        ]
    );
}

#[test]
fn a_fetch_brings_the_bytes_the_newest_commit_names() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let mut arm = the_arm_project(parent.path());
    let remote = remote_server.remote(Some(REMOTE_BEARER_TOKEN));
    arm.push_to_remote(&remote).unwrap();
    let clone =
        ProjectRepository::clone_from_remote(&parent.path().join("clone"), &remote).unwrap();

    import(&mut arm, parent.path(), "outro.mp4", b"more video");
    arm.push_to_remote(&remote).unwrap();
    let outro_sha256 = content_sha256_of_the_asset_at(&arm, "media/outro.mp4");
    assert!(clone.content_store().stored_content(&outro_sha256).is_err());

    assert_eq!(
        clone.fetch_from_remote(&remote).unwrap(),
        arm.history().head_commit().unwrap()
    );
    assert_eq!(
        std::fs::read(clone.content_store().content_path(&outro_sha256)).unwrap(),
        b"more video"
    );
}

#[test]
fn bytes_that_do_not_hash_to_their_name_are_refused_and_the_clone_leaves_nothing() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = the_arm_project(parent.path());
    let remote = remote_server.remote(Some(REMOTE_BEARER_TOKEN));
    arm.push_to_remote(&remote).unwrap();
    remote_server
        .lfs_endpoint_held
        .lock()
        .unwrap()
        .serve_other_bytes = Some(b"tampered bytes".to_vec());

    let clone_directory = parent.path().join("clone");
    let refused = ProjectRepository::clone_from_remote(&clone_directory, &remote)
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::ContentArrivedAsOtherBytes { .. }
        ),
        "{refused:?}"
    );
    assert!(!clone_directory.exists());
}

#[test]
fn a_clone_from_a_remote_that_lacks_the_bytes_is_refused_naming_them() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = the_arm_project(parent.path());
    let remote = remote_server.remote(Some(REMOTE_BEARER_TOKEN));
    arm.history().push_main_to(&remote).unwrap();
    let intro_sha256 = content_sha256_of_the_asset_at(&arm, "media/intro.mp4");

    let refused = ProjectRepository::clone_from_remote(&parent.path().join("clone"), &remote)
        .err()
        .unwrap();
    assert!(
        matches!(
            &refused,
            ProjectRepositoryRefusal::ContentRemoteHoldsNoSuchContent { content_sha256, .. }
                if *content_sha256 == intro_sha256
        ),
        "{refused:?}"
    );
}

#[test]
fn a_clone_is_never_written_into_a_folder_that_holds_files() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = the_arm_project(parent.path());
    let remote = remote_server.remote(Some(REMOTE_BEARER_TOKEN));
    arm.push_to_remote(&remote).unwrap();
    let occupied = parent.path().join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("notes.txt"), b"mine").unwrap();

    assert!(matches!(
        ProjectRepository::clone_from_remote(&occupied, &remote),
        Err(ProjectRepositoryRefusal::CloneDestinationNotEmpty { .. })
    ));
    assert_eq!(std::fs::read(occupied.join("notes.txt")).unwrap(), b"mine");
    assert!(!occupied.join(PROJECT_HISTORY_DIRECTORY).exists());
}

#[test]
fn a_push_without_the_token_sends_neither_bytes_nor_history() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = the_arm_project(parent.path());
    let refused = arm
        .push_to_remote(&remote_server.remote(Some("art_v1_wrong")))
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::ContentRemoteRefused { .. }
        ),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("refused the token"),
        "{refused}"
    );
    assert!(!refused.to_string().contains("art_v1_wrong"), "{refused}");
    assert!(remote_server.requests().is_empty());
}
