// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use serde_json::Map;
use streamlib_project_repository::{
    ProjectActionApplyOutcome, ProjectAssetAction, ProjectAssetImportSource, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionReversal,
};

const A_PNG_HEADER_AND_HIGH_BYTES: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff, 0xfe, 0x80, 0x00,
];

struct AProjectWithNodeCode {
    root: tempfile::TempDir,
    project_directory: PathBuf,
    repository: ProjectRepository,
}

impl AProjectWithNodeCode {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("studio");
        let repository =
            ProjectRepository::create(&project_directory, "studio", &a_person()).unwrap();
        Self {
            root,
            project_directory,
            repository,
        }
    }

    fn write(&mut self, path: &str, content: &[u8]) -> Result<(), ProjectRepositoryRefusal> {
        self.repository
            .write_project_file(&a_person(), None, path, content)
            .map(|outcome| assert!(matches!(outcome, ProjectActionApplyOutcome::AppliedNow(_))))
    }

    fn on_disk(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.project_directory.join(path)).ok()
    }

    fn undo(&mut self) {
        let reversal = self.repository.undo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .unwrap();
    }

    fn redo(&mut self) {
        let reapplied = self.repository.redo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                reapplied.operations,
                reapplied.lineage,
            )
            .unwrap();
    }
}

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jo".to_owned(),
    }
}

#[test]
fn a_written_file_is_listed_read_back_byte_for_byte_and_recorded_in_history() {
    let mut project = AProjectWithNodeCode::new();

    project
        .write("views/panel/logo.png", A_PNG_HEADER_AND_HIGH_BYTES)
        .unwrap();
    project
        .write("nodes/camera.py", b"class Camera: ...\n")
        .unwrap();

    assert_eq!(
        project
            .repository
            .read_project_file("views/panel/logo.png")
            .unwrap(),
        A_PNG_HEADER_AND_HIGH_BYTES
    );
    let listed: Vec<(String, u64)> = project
        .repository
        .list_project_files(Some("nodes"))
        .unwrap()
        .into_iter()
        .map(|listed| (listed.path, listed.byte_len))
        .collect();
    assert_eq!(listed, [("nodes/camera.py".to_owned(), 18)]);
    let files_at_head = project.repository.history().files_at_head().unwrap();
    assert_eq!(
        files_at_head.get("views/panel/logo.png").map(Vec::as_slice),
        Some(A_PNG_HEADER_AND_HIGH_BYTES)
    );
    assert_eq!(
        project.repository.history().commits_newest_first().unwrap()[0]
            .message
            .lines()
            .next()
            .unwrap(),
        "write_project_file nodes/camera.py"
    );
    assert!(
        !project
            .project_directory
            .join(".tatolab/staged-file-writes")
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_some()),
        "no staged copy is left behind"
    );
}

#[test]
fn an_undo_puts_back_what_a_file_held_or_takes_a_new_file_out_and_a_redo_writes_it_again() {
    let mut project = AProjectWithNodeCode::new();
    project.write("nodes/camera.py", b"first\n").unwrap();
    project.write("nodes/camera.py", b"second\n").unwrap();

    project.undo();
    assert_eq!(project.on_disk("nodes/camera.py").unwrap(), b"first\n");

    project.redo();
    assert_eq!(project.on_disk("nodes/camera.py").unwrap(), b"second\n");

    project.write("nodes/new.py", b"new\n").unwrap();
    project.undo();
    assert_eq!(project.on_disk("nodes/new.py"), None);
    project.redo();
    assert_eq!(project.on_disk("nodes/new.py").unwrap(), b"new\n");
}

#[test]
fn an_undo_puts_back_what_the_file_held_on_disk_when_it_was_written_over() {
    let mut project = AProjectWithNodeCode::new();
    project.write("nodes/camera.py", b"first\n").unwrap();
    std::fs::write(
        project.project_directory.join("nodes/camera.py"),
        b"edited by hand\n",
    )
    .unwrap();
    std::fs::write(
        project.project_directory.join("nodes/made_by_hand.py"),
        b"by hand\n",
    )
    .unwrap();

    project.write("nodes/camera.py", b"second\n").unwrap();
    project.undo();
    assert_eq!(
        project.on_disk("nodes/camera.py").unwrap(),
        b"edited by hand\n"
    );

    project
        .write("nodes/made_by_hand.py", b"written\n")
        .unwrap();
    project.undo();
    assert_eq!(
        project.on_disk("nodes/made_by_hand.py").unwrap(),
        b"by hand\n"
    );
}

#[test]
fn an_undo_or_a_redo_is_refused_once_the_file_changed_after_it_was_recorded() {
    let mut project = AProjectWithNodeCode::new();
    project.write("nodes/camera.py", b"first\n").unwrap();
    project
        .repository
        .write_project_file(&an_agent(), None, "nodes/camera.py", b"by the agent\n")
        .unwrap();
    let undo = project.repository.undo_by(&a_person());
    assert_the_reversal_is_refused(&mut project, undo);

    let mut project = AProjectWithNodeCode::new();
    project.write("nodes/camera.py", b"first\n").unwrap();
    project.write("nodes/camera.py", b"second\n").unwrap();
    project.undo();
    std::fs::write(
        project.project_directory.join("nodes/camera.py"),
        b"edited by hand\n",
    )
    .unwrap();
    let redo = project.repository.redo_by(&a_person());
    assert_the_reversal_is_refused(&mut project, redo);
    assert_eq!(
        project.on_disk("nodes/camera.py").unwrap(),
        b"edited by hand\n"
    );
}

fn assert_the_reversal_is_refused(
    project: &mut AProjectWithNodeCode,
    reversal: Result<ProjectTransactionReversal, ProjectRepositoryRefusal>,
) {
    let reversal = reversal.unwrap();
    assert!(matches!(
        project.repository.apply_project_operations(
            &a_person(),
            None,
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        ),
        Err(ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRecorded { .. })
    ));
}

fn an_agent() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Agent,
        name: "an agent".to_owned(),
    }
}

#[test]
fn the_same_content_is_an_empty_transaction() {
    let mut project = AProjectWithNodeCode::new();
    project.write("stream.py", b"same\n").unwrap();

    assert!(matches!(
        project.write("stream.py", b"same\n"),
        Err(ProjectRepositoryRefusal::EmptyTransaction)
    ));
}

#[test]
fn an_undo_is_refused_once_the_file_it_would_take_out_is_gone() {
    let mut project = AProjectWithNodeCode::new();
    project.write("nodes/new.py", b"new\n").unwrap();
    std::fs::remove_file(project.project_directory.join("nodes/new.py")).unwrap();

    let reversal = project.repository.undo_by(&a_person()).unwrap();
    assert!(matches!(
        project.repository.apply_project_operations(
            &a_person(),
            None,
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        ),
        Err(ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRecorded { .. })
    ));
}

#[test]
fn a_path_outside_the_project_or_through_a_link_is_refused_for_every_verb() {
    let mut project = AProjectWithNodeCode::new();
    let outside = project.root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, project.project_directory.join("linked")).unwrap();

    for refused in [
        "../outside/secret.txt",
        "/etc/passwd",
        "linked/secret.txt",
        ".tatolab/history.git/HEAD",
        ".TATOLAB/history.git/HEAD",
    ] {
        assert!(
            matches!(
                project.repository.read_project_file(refused),
                Err(ProjectRepositoryRefusal::ProjectFilePathRefused { .. })
            ),
            "read {refused}"
        );
        assert!(
            matches!(
                project.write(refused, b"x"),
                Err(ProjectRepositoryRefusal::ProjectFilePathRefused { .. })
            ),
            "write {refused}"
        );
    }
    assert!(matches!(
        project.repository.list_project_files(Some("linked")),
        Err(ProjectRepositoryRefusal::ProjectFilePathRefused { .. })
    ));
    assert_eq!(
        std::fs::read(outside.join("secret.txt")).unwrap(),
        b"secret"
    );
    assert!(matches!(
        project.repository.read_project_file("nodes/missing.py"),
        Err(ProjectRepositoryRefusal::NoSuchProjectFile(_))
    ));
}

#[test]
fn a_file_another_verb_changes_or_history_does_not_record_is_never_written() {
    let mut project = AProjectWithNodeCode::new();
    let take = project.root.path().join("take.mp4");
    std::fs::write(&take, b"take").unwrap();
    project
        .repository
        .apply_asset_changes(
            &a_person(),
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(take),
                asset_path: Some("media/intro.mp4".to_owned()),
                file_name: None,
                kind: None,
                import_settings: Map::new(),
            }],
        )
        .unwrap();
    let commits_before = project.repository.history().commits_newest_first().unwrap();

    for refused in [
        "streams/main.graph.json",
        "media/intro.mp4.asset.json",
        "media/intro.mp4",
        "tatolab-project.json",
        "Tatolab-Project.json",
        "Media/Intro.mp4",
        "Streams/Main.Graph.json",
        "imports/other/stream.py",
        "Imports/other/stream.py",
        ".venv/lib/site.py",
        "nodes/__pycache__/camera.pyc",
        "views/panel/node_modules/left-pad/index.js",
    ] {
        assert!(
            matches!(
                project.write(refused, b"x"),
                Err(ProjectRepositoryRefusal::ProjectFileWrittenByAnotherAction { .. })
            ),
            "{refused}"
        );
    }
    assert_eq!(
        project.repository.history().commits_newest_first().unwrap(),
        commits_before
    );
    assert_eq!(project.on_disk("media/intro.mp4").unwrap(), b"take");
}
