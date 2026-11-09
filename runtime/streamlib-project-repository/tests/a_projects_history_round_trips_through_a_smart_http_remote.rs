// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's history pushed to a Git smart-HTTP remote and fetched back unchanged, over the
//! crate's own transport, against the remote `support` serves.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use support::{REMOTE_BEARER_TOKEN, SmartHttpRemoteServedByGitHttpBackend};

use streamlib_project_repository::{
    ProjectAssetChangeRequest, ProjectAssetImportSource, ProjectHistoryRepository,
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, the_content_path_of_a_dependency_asset,
};

fn a_project_with_one_imported_asset(parent: &Path) -> ProjectRepository {
    let project_directory = parent.join("arm-control");
    let mut project = ProjectRepository::create(
        &project_directory,
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    std::fs::write(parent.join("intro.mp4"), b"video bytes").unwrap();
    project
        .apply_asset_changes(
            &ProjectTransactionAuthor {
                kind: ProjectTransactionAuthorKind::Person,
                name: "Jonathan".to_owned(),
            },
            vec![ProjectAssetChangeRequest::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(parent.join("intro.mp4")),
                asset_path: "media/intro.mp4".to_owned(),
                kind: None,
                import_settings: serde_json::Map::new(),
            }],
        )
        .unwrap();
    project
}

#[test]
fn a_pushed_history_is_fetched_back_unchanged_into_a_fresh_history() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let project = a_project_with_one_imported_asset(parent.path());

    project
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let pushed_head = project.history().head_commit().unwrap().unwrap();
    let remote_head = Command::new("git")
        .args(["rev-parse", "refs/heads/main"])
        .current_dir(remote_server.bare_repository())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&remote_head.stdout).trim(),
        pushed_head
    );

    let fetched_into = tempfile::tempdir().unwrap();
    let fresh_history = ProjectHistoryRepository::create_for_project(fetched_into.path()).unwrap();
    let fetched_head = fresh_history
        .fetch_main_from(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    assert_eq!(fetched_head.as_deref(), Some(pushed_head.as_str()));
    assert_eq!(
        fresh_history.commits_newest_first().unwrap(),
        project.history().commits_newest_first().unwrap()
    );
    assert_eq!(
        fresh_history.files_at_head().unwrap(),
        project.history().files_at_head().unwrap()
    );
}

#[test]
fn a_push_without_the_token_is_refused_in_the_remotes_words() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let project = a_project_with_one_imported_asset(parent.path());
    let refused = project
        .history()
        .push_main_to(&remote_server.remote(Some("art_v1_wrong")))
        .err()
        .unwrap();
    assert!(
        matches!(refused, ProjectRepositoryRefusal::History(_)),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("refused the token"),
        "{refused}"
    );
    assert!(!refused.to_string().contains("art_v1_wrong"), "{refused}");
}

#[test]
fn a_push_that_is_not_a_fast_forward_is_refused() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let first_parent = tempfile::tempdir().unwrap();
    let first = a_project_with_one_imported_asset(first_parent.path());
    first
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let second_parent = tempfile::tempdir().unwrap();
    let unrelated = a_project_with_one_imported_asset(second_parent.path());
    let refused = unrelated
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::HistoryRemoteRefused { .. }
                | ProjectRepositoryRefusal::History(_)
        ),
        "{refused:?}"
    );
}

#[test]
fn a_dependency_on_a_remote_that_needs_a_token_fetches_with_it_and_keeps_it_nowhere() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let arm = a_project_with_one_imported_asset(parent.path());
    arm.history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let first_revision = arm.history().head_commit().unwrap().unwrap();
    let remote_url = remote_server.remote(None).url;
    let mut cell = ProjectRepository::create(
        &parent.path().join("cell"),
        "cell",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    let a_person = ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jonathan".to_owned(),
    };

    let refused = cell
        .add_dependency(&a_person, &remote_url, "main", Some("art_v1_wrong"))
        .err()
        .unwrap();
    assert!(!refused.to_string().contains("art_v1_wrong"), "{refused}");
    assert!(cell.project_file().dependencies.is_empty());

    cell.add_dependency(&a_person, &remote_url, "main", Some(REMOTE_BEARER_TOKEN))
        .unwrap();
    let arm_project = arm.project_file().project;
    assert_eq!(
        cell.dependency(arm_project).unwrap().revision,
        first_revision
    );
    let intro = arm
        .assets()
        .unwrap()
        .into_iter()
        .find(|asset| asset.path.to_string() == "media/intro.mp4")
        .unwrap()
        .asset;
    assert!(matches!(
        the_content_path_of_a_dependency_asset(cell.project_directory(), arm_project, intro),
        Err(ProjectRepositoryRefusal::DependencyAssetContentNotFetched { .. })
    ));

    std::fs::write(parent.path().join("outro.mp4"), b"more video").unwrap();
    let mut arm = ProjectRepository::open(arm.project_directory()).unwrap();
    arm.apply_asset_changes(
        &a_person,
        vec![ProjectAssetChangeRequest::ImportAsset {
            source: ProjectAssetImportSource::FileOnThisMachine(parent.path().join("outro.mp4")),
            asset_path: "media/outro.mp4".to_owned(),
            kind: None,
            import_settings: serde_json::Map::new(),
        }],
    )
    .unwrap();
    arm.history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    assert!(
        cell.update_dependency(&a_person, arm_project, "main", None)
            .is_err()
    );
    cell.update_dependency(&a_person, arm_project, "main", Some(REMOTE_BEARER_TOKEN))
        .unwrap();
    assert_eq!(
        cell.dependency(arm_project).unwrap().revision,
        arm.history().head_commit().unwrap().unwrap()
    );

    let every_file_cell_keeps = walk_every_file(cell.project_directory());
    for file in every_file_cell_keeps {
        let bytes = std::fs::read(&file).unwrap();
        assert!(
            !bytes
                .windows(REMOTE_BEARER_TOKEN.len())
                .any(|window| window == REMOTE_BEARER_TOKEN.as_bytes()),
            "{} holds the token",
            file.display()
        );
    }
}

fn walk_every_file(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_every_file(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// Against a real Artifacts repo holding a project, named by `TATOLAB_ARTIFACTS_TEST_REMOTE_URL`
/// with a read token in `TATOLAB_ARTIFACTS_TEST_REMOTE_TOKEN`; passes without checking when
/// either is unset.
#[test]
fn a_dependency_on_an_artifacts_repo_fetches_with_its_token_where_one_is_present() {
    let (Ok(remote_url), Ok(bearer_token)) = (
        std::env::var("TATOLAB_ARTIFACTS_TEST_REMOTE_URL"),
        std::env::var("TATOLAB_ARTIFACTS_TEST_REMOTE_TOKEN"),
    ) else {
        return;
    };
    let parent = tempfile::tempdir().unwrap();
    let mut cell = ProjectRepository::create(
        &parent.path().join("cell"),
        "cell",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    let applied = cell
        .add_dependency(
            &ProjectTransactionAuthor {
                kind: ProjectTransactionAuthorKind::Person,
                name: "Jonathan".to_owned(),
            },
            &remote_url,
            "main",
            Some(&bearer_token),
        )
        .unwrap();
    assert_eq!(cell.project_file().dependencies.len(), 1);
    assert!(!applied.commit.is_empty());
}
