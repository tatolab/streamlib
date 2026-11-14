// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use serde_json::Map;
use streamlib_project_repository::{
    AppliedProjectTransaction, PROJECT_UPLOADS_DIRECTORY, ProjectAssetAction, ProjectAssetContent,
    ProjectAssetImportSource, ProjectAssetKind, ProjectAssetOperation, ProjectOperation,
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, the_file_of_an_asset, the_upload_file_of,
};
use uuid::Uuid;

struct AProjectHoldingOneClip {
    _root: tempfile::TempDir,
    project_directory: PathBuf,
    import_sources_directory: PathBuf,
    repository: ProjectRepository,
    clip: Uuid,
}

impl AProjectHoldingOneClip {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("studio");
        let import_sources_directory = root.path().join("downloads");
        std::fs::create_dir_all(&import_sources_directory).unwrap();
        let mut repository =
            ProjectRepository::create(&project_directory, "studio", &a_person()).unwrap();
        let take_one = import_sources_directory.join("take-one.mp4");
        std::fs::write(&take_one, b"take one").unwrap();
        let imported = repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::ImportAsset {
                    source: ProjectAssetImportSource::FileOnThisMachine(take_one),
                    asset_path: Some("media/intro.mp4".to_owned()),
                    file_name: None,
                    kind: None,
                    import_settings: Map::new(),
                }],
            )
            .unwrap()
            .into_applied_transaction();
        let clip = match &imported.transaction.operations[0] {
            ProjectOperation::Asset(ProjectAssetOperation::ImportAsset { asset }) => asset.asset,
            other => panic!("expected an import, got {other:?}"),
        };
        Self {
            _root: root,
            project_directory,
            import_sources_directory,
            repository,
            clip,
        }
    }

    fn a_file_on_this_machine(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.import_sources_directory.join(file_name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn replace_the_clip_with(
        &mut self,
        source: ProjectAssetImportSource,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        self.repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::ReplaceAsset {
                    asset: self.clip,
                    source,
                }],
            )
            .map(|outcome| outcome.into_applied_transaction())
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

    fn the_clip_as_its_sidecar_records_it(&self) -> ProjectAssetContent {
        let clip = ProjectRepository::open(&self.project_directory)
            .unwrap()
            .asset(self.clip)
            .unwrap();
        ProjectAssetContent {
            content_sha256: clip.content_sha256,
            content_byte_len: clip.content_byte_len,
        }
    }

    fn the_bytes_a_node_reads(&self) -> Vec<u8> {
        std::fs::read(the_file_of_an_asset(&self.project_directory, self.clip).unwrap()).unwrap()
    }
}

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jo".to_owned(),
    }
}

fn content_of(bytes: &[u8]) -> ProjectAssetContent {
    use sha2::Digest;
    ProjectAssetContent {
        content_sha256: sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        content_byte_len: bytes.len() as u64,
    }
}

fn files_under(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

#[test]
fn a_replaced_asset_keeps_its_uuid_and_path_and_its_sidecar_records_the_new_bytes() {
    let mut project = AProjectHoldingOneClip::new();
    let take_two = project.a_file_on_this_machine("take-two.mp4", b"take two, longer");

    let replaced = project
        .replace_the_clip_with(ProjectAssetImportSource::FileOnThisMachine(take_two))
        .unwrap();

    assert_eq!(
        replaced.transaction.operations,
        [ProjectOperation::Asset(
            ProjectAssetOperation::ReplaceAssetContent {
                asset: project.clip,
                path: project.repository.asset(project.clip).unwrap().path,
                from: content_of(b"take one"),
                to: content_of(b"take two, longer"),
            }
        )]
    );
    let clip = project.repository.asset(project.clip).unwrap();
    assert_eq!(clip.path.as_str(), "media/intro.mp4");
    assert_eq!(clip.kind, ProjectAssetKind::Media);
    assert_eq!(
        project.the_clip_as_its_sidecar_records_it(),
        content_of(b"take two, longer")
    );
    assert_eq!(project.the_bytes_a_node_reads(), b"take two, longer");
    let files_at_head = project.repository.history().files_at_head().unwrap();
    assert!(files_at_head.contains_key("media/intro.mp4.asset.json"));
    assert!(!files_at_head.contains_key("media/intro.mp4"));
}

#[test]
fn an_undo_restores_the_earlier_sidecar_and_bytes_and_a_redo_the_replacement() {
    let mut project = AProjectHoldingOneClip::new();
    let take_two = project.a_file_on_this_machine("take-two.mp4", b"take two");
    project
        .replace_the_clip_with(ProjectAssetImportSource::FileOnThisMachine(take_two))
        .unwrap();

    project.undo();

    assert_eq!(
        project.the_clip_as_its_sidecar_records_it(),
        content_of(b"take one")
    );
    assert_eq!(project.the_bytes_a_node_reads(), b"take one");

    project.redo();

    assert_eq!(
        project.the_clip_as_its_sidecar_records_it(),
        content_of(b"take two")
    );
    assert_eq!(project.the_bytes_a_node_reads(), b"take two");
}

#[test]
fn an_upload_replaces_an_asset_by_its_hash_and_leaves_the_uploads() {
    let mut project = AProjectHoldingOneClip::new();
    let upload = the_upload_file_of(
        &project.project_directory,
        &content_of(b"uploaded take").content_sha256,
    );
    std::fs::create_dir_all(upload.parent().unwrap()).unwrap();
    std::fs::write(&upload, b"uploaded take").unwrap();

    project
        .replace_the_clip_with(ProjectAssetImportSource::UploadedContent(
            content_of(b"uploaded take").content_sha256,
        ))
        .unwrap();

    assert!(!upload.exists());
    assert_eq!(project.the_bytes_a_node_reads(), b"uploaded take");
}

#[test]
fn the_same_bytes_change_nothing_and_a_refused_replacement_leaves_no_copy_behind() {
    let mut project = AProjectHoldingOneClip::new();
    let same_take = project.a_file_on_this_machine("same.mp4", b"take one");
    let commits_before = project.repository.history().commits_newest_first().unwrap();

    assert!(matches!(
        project.replace_the_clip_with(ProjectAssetImportSource::FileOnThisMachine(same_take)),
        Err(ProjectRepositoryRefusal::EmptyTransaction)
    ));
    assert!(matches!(
        project.replace_the_clip_with(ProjectAssetImportSource::FileOnThisMachine(
            project.import_sources_directory.join("never-written.mp4"),
        )),
        Err(ProjectRepositoryRefusal::ImportSourceUnreadable { .. })
    ));
    assert_eq!(
        project.repository.history().commits_newest_first().unwrap(),
        commits_before
    );
    assert!(
        files_under(&project.project_directory.join(PROJECT_UPLOADS_DIRECTORY)).is_empty(),
        "{:?}",
        files_under(&project.project_directory.join(PROJECT_UPLOADS_DIRECTORY))
    );
    assert_eq!(project.the_bytes_a_node_reads(), b"take one");
}

#[test]
fn a_stream_asset_and_an_asset_the_project_does_not_hold_are_never_replaced() {
    let mut project = AProjectHoldingOneClip::new();
    let take_two = project.a_file_on_this_machine("take-two.mp4", b"take two");
    let stream = match &project
        .repository
        .apply_asset_changes(
            &a_person(),
            None,
            vec![ProjectAssetAction::CreateStream {
                path: "streams/preview.graph.json".to_owned(),
                display_name: "Preview".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction()
        .transaction
        .operations[0]
    {
        ProjectOperation::StreamDocument(
            streamlib_project_repository::ProjectStreamDocumentOperation::CreateStreamDocument {
                stream,
                ..
            },
        ) => *stream,
        other => panic!("expected a created stream document, got {other:?}"),
    };

    for (asset, refused_as) in [(stream, "stream"), (Uuid::new_v4(), "no such asset")] {
        let refusal = project
            .repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::ReplaceAsset {
                    asset,
                    source: ProjectAssetImportSource::FileOnThisMachine(take_two.clone()),
                }],
            )
            .map(|_| ())
            .unwrap_err();
        match (refused_as, refusal) {
            ("stream", ProjectRepositoryRefusal::StreamAssetsAreNotReplaced(refused)) => {
                assert_eq!(refused, stream);
            }
            ("no such asset", ProjectRepositoryRefusal::NoSuchAsset(refused)) => {
                assert_eq!(refused, asset);
            }
            (expected, refusal) => panic!("expected {expected}, got {refusal:?}"),
        }
    }
}
