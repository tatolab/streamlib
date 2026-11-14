// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_project_repository::{
    ProjectAssetAction, ProjectAssetKind, ProjectOperation, ProjectRepository,
    ProjectRepositoryRefusal, ProjectStreamDocumentOperation, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind,
};
use uuid::Uuid;

struct AProjectOnDisk {
    _root: tempfile::TempDir,
    repository: ProjectRepository,
}

impl AProjectOnDisk {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("studio");
        let repository = ProjectRepository::create(
            &project_directory,
            "studio",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        Self {
            _root: root,
            repository,
        }
    }

    fn apply(
        &mut self,
        action: ProjectAssetAction,
    ) -> Result<streamlib_project_repository::AppliedProjectTransaction, ProjectRepositoryRefusal>
    {
        self.repository
            .apply_asset_changes(&a_person(), None, vec![action])
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

    fn created_stream(&mut self, path: &str, display_name: &str) -> Uuid {
        let applied = self
            .apply(ProjectAssetAction::CreateStream {
                path: path.to_owned(),
                display_name: display_name.to_owned(),
            })
            .unwrap();
        match &applied.transaction.operations[..] {
            [
                ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::CreateStreamDocument { stream, .. },
                ),
            ] => *stream,
            other => panic!("a create records one created document, not {other:?}"),
        }
    }

    fn stream_assets(&self) -> Vec<Uuid> {
        self.repository
            .assets()
            .unwrap()
            .into_iter()
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)
            .map(|asset| asset.asset)
            .collect()
    }

    fn display_name_of(&self, stream: Uuid) -> Option<String> {
        ProjectRepository::open(self.repository.project_directory())
            .unwrap()
            .stream_graph_document(stream)
            .unwrap()
            .display_name()
            .map(str::to_owned)
    }
}

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Jo".to_owned(),
    }
}

#[test]
fn a_created_stream_is_a_stream_asset_shown_under_its_display_name_until_its_undo() {
    let mut project = AProjectOnDisk::new();

    let stream = project.created_stream("streams/preview.graph.json", "  Preview  ");

    assert_eq!(project.stream_assets(), vec![stream]);
    assert_eq!(project.display_name_of(stream).as_deref(), Some("Preview"));
    let document = project.repository.stream_graph_document(stream).unwrap();
    assert!(document.nodes().is_empty());
    assert!(document.links().is_empty());

    project.undo();

    assert!(project.stream_assets().is_empty());
    assert!(
        !project
            .repository
            .project_directory()
            .join("streams/preview.graph.json")
            .exists()
    );
}

#[test]
fn a_deleted_stream_leaves_the_project_and_its_undo_restores_it_as_it_was() {
    let mut project = AProjectOnDisk::new();
    let stream = project.created_stream("streams/preview.graph.json", "Preview");

    project
        .apply(ProjectAssetAction::DeleteStream {
            stream_asset: stream,
        })
        .unwrap();
    assert!(project.stream_assets().is_empty());

    project.undo();

    assert_eq!(project.stream_assets(), vec![stream]);
    assert_eq!(project.display_name_of(stream).as_deref(), Some("Preview"));
}

#[test]
fn a_display_name_is_written_in_the_graph_document_and_its_undo_writes_the_earlier_one() {
    let mut project = AProjectOnDisk::new();
    let stream = project.created_stream("streams/preview.graph.json", "Preview");

    project
        .apply(ProjectAssetAction::SetStreamDisplayName {
            stream_asset: stream,
            display_name: "Front camera".to_owned(),
        })
        .unwrap();
    assert_eq!(
        project.display_name_of(stream).as_deref(),
        Some("Front camera")
    );

    project.undo();

    assert_eq!(project.display_name_of(stream).as_deref(), Some("Preview"));
}

#[test]
fn a_display_name_that_shows_nothing_and_a_stream_the_project_does_not_hold_are_refused() {
    let mut project = AProjectOnDisk::new();
    let stream = project.created_stream("streams/preview.graph.json", "Preview");

    assert!(matches!(
        project.apply(ProjectAssetAction::SetStreamDisplayName {
            stream_asset: stream,
            display_name: "   ".to_owned(),
        }),
        Err(ProjectRepositoryRefusal::DisplayNameShowsNothing(_))
    ));
    assert!(matches!(
        project.apply(ProjectAssetAction::CreateStream {
            path: "streams/empty.graph.json".to_owned(),
            display_name: String::new(),
        }),
        Err(ProjectRepositoryRefusal::DisplayNameShowsNothing(_))
    ));
    let unheld = Uuid::new_v4();
    assert!(matches!(
        project.apply(ProjectAssetAction::DeleteStream {
            stream_asset: unheld
        }),
        Err(ProjectRepositoryRefusal::NoSuchStream(asset)) if asset == unheld
    ));
}
