// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, json};
use streamlib_project_repository::{
    ProjectAssetChangeRequest, ProjectAssetImportSource, ProjectOperation, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionLineage, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphOperation, StreamGraphOperationOnAStream,
};
use uuid::Uuid;

struct ProjectHoldingOneStream {
    _root: tempfile::TempDir,
    repository: ProjectRepository,
    stream: Uuid,
}

impl ProjectHoldingOneStream {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let stream = Uuid::new_v4();
        std::fs::create_dir_all(project_directory.join("streams")).unwrap();
        std::fs::write(
            project_directory.join("streams/main.graph.json"),
            json!({ "asset": stream, "nodes": [], "links": [] }).to_string(),
        )
        .unwrap();
        Self {
            repository: ProjectRepository::open(&project_directory).unwrap(),
            _root: root,
            stream,
        }
    }

    fn on_the_stream(&self, graph_operation: StreamGraphOperation) -> ProjectOperation {
        ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
            stream: self.stream,
            graph_operation,
        })
    }

    fn reopened_document_bytes(&self) -> Vec<u8> {
        ProjectRepository::open(self.repository.project_directory())
            .unwrap()
            .stream_graph_document(self.stream)
            .unwrap()
            .canonical_bytes()
            .unwrap()
    }

    fn document_bytes(&self) -> Vec<u8> {
        self.repository
            .stream_graph_document(self.stream)
            .unwrap()
            .canonical_bytes()
            .unwrap()
    }

    fn apply(
        &mut self,
        author: &ProjectTransactionAuthor,
        operations: Vec<ProjectOperation>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        self.repository
            .apply_project_operations(author, operations, ProjectTransactionLineage::NewEdit)
            .map(|_| ())
    }

    fn undo(&mut self, author: &ProjectTransactionAuthor) -> Result<(), ProjectRepositoryRefusal> {
        let reversal = self.repository.undo_by(author)?;
        self.repository
            .apply_project_operations(author, reversal.operations, reversal.lineage)
            .map(|_| ())
    }

    fn redo(&mut self, author: &ProjectTransactionAuthor) -> Result<(), ProjectRepositoryRefusal> {
        let reversal = self.repository.redo_by(author)?;
        self.repository
            .apply_project_operations(author, reversal.operations, reversal.lineage)
            .map(|_| ())
    }
}

fn an_author(kind: ProjectTransactionAuthorKind, name: &str) -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind,
        name: name.to_owned(),
    }
}

fn a_node(name: &str, config: serde_json::Value) -> StreamGraphDocumentNode {
    StreamGraphDocumentNode {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        node_type: format!("nodes.{name}:Node"),
        config,
        pins: Map::new(),
        studio: Map::new(),
        fragment: None,
    }
}

fn add(node: &StreamGraphDocumentNode) -> StreamGraphOperation {
    StreamGraphOperation::AddNode {
        node: node.clone(),
        links: Vec::new(),
    }
}

fn a_link(
    source: &StreamGraphDocumentNode,
    target: &StreamGraphDocumentNode,
) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source.id,
            port: "video".to_owned(),
        },
        target: StreamGraphDocumentLinkEnd {
            node: target.id,
            port: "video_in".to_owned(),
        },
    }
}

#[test]
fn five_edits_batched_in_one_transaction_are_kept_and_one_undo_reverses_them_all() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let commits_before = project
        .repository
        .history()
        .commits_newest_first()
        .unwrap()
        .len();
    let empty_document = project.document_bytes();
    let source = a_node("source", json!({}));
    let effect = a_node("effect", json!({}));
    let operations = vec![
        project.on_the_stream(add(&source)),
        project.on_the_stream(add(&effect)),
        project.on_the_stream(StreamGraphOperation::Connect {
            link: a_link(&source, &effect),
        }),
        project.on_the_stream(StreamGraphOperation::SetNodeConfig {
            node: effect.id,
            from: json!({}),
            to: json!({ "strength": 0.75 }),
        }),
        project.on_the_stream(StreamGraphOperation::SetPinValue {
            node: effect.id,
            input: "threshold".to_owned(),
            from: None,
            to: Some(json!(0.25)),
        }),
    ];
    project.apply(&agent, operations).unwrap();

    assert_eq!(
        project
            .repository
            .history()
            .commits_newest_first()
            .unwrap()
            .len(),
        commits_before + 1,
        "the batch is one commit"
    );
    let edited_document = project.reopened_document_bytes();
    let reopened = ProjectRepository::open(project.repository.project_directory())
        .unwrap()
        .stream_graph_document(project.stream)
        .unwrap();
    let effect_as_kept = reopened.node(effect.id).unwrap();
    assert_eq!(effect_as_kept.config, json!({ "strength": 0.75 }));
    assert_eq!(effect_as_kept.pins["threshold"], json!(0.25));
    assert_eq!(reopened.links(), [a_link(&source, &effect)]);

    project.undo(&agent).unwrap();
    assert_eq!(project.reopened_document_bytes(), empty_document);
    project.redo(&agent).unwrap();
    assert_eq!(project.reopened_document_bytes(), edited_document);
    project.undo(&agent).unwrap();
    assert_eq!(project.reopened_document_bytes(), empty_document);
    project.undo(&agent).unwrap_err();
}

#[test]
fn each_authors_undo_reverses_only_that_authors_own_edits() {
    let mut project = ProjectHoldingOneStream::new();
    let person = an_author(ProjectTransactionAuthorKind::Person, "Jonathan");
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let source = a_node("source", json!({}));
    let meter = a_node("meter", json!({}));
    project
        .apply(&person, vec![project.on_the_stream(add(&source))])
        .unwrap();
    project
        .apply(&agent, vec![project.on_the_stream(add(&meter))])
        .unwrap();
    project
        .apply(
            &person,
            vec![project.on_the_stream(StreamGraphOperation::RenameNode {
                node: source.id,
                from: "source".to_owned(),
                to: "camera".to_owned(),
            })],
        )
        .unwrap();
    project
        .apply(
            &agent,
            vec![
                project.on_the_stream(StreamGraphOperation::SetStudioFields {
                    node: meter.id,
                    from: Map::new(),
                    to: json!({ "position": [120, 40] })
                        .as_object()
                        .unwrap()
                        .clone(),
                }),
            ],
        )
        .unwrap();

    project.undo(&person).unwrap();
    let document = project
        .repository
        .stream_graph_document(project.stream)
        .unwrap();
    assert_eq!(document.node(source.id).unwrap().name, "source");
    assert_eq!(
        document.node(meter.id).unwrap().studio["position"],
        json!([120, 40]),
        "the person's undo leaves the agent's edit"
    );

    project.undo(&agent).unwrap();
    project.undo(&agent).unwrap();
    let document = project
        .repository
        .stream_graph_document(project.stream)
        .unwrap();
    document.node(meter.id).unwrap_err();
    assert_eq!(
        document.node(source.id).unwrap().name,
        "source",
        "the agent's undos leave the person's node"
    );
    assert!(matches!(
        project.undo(&agent),
        Err(ProjectRepositoryRefusal::NothingToUndo { .. })
    ));
}

#[test]
fn a_new_edit_after_an_undo_leaves_nothing_to_redo() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_node("first", json!({}))))],
        )
        .unwrap();
    project.undo(&agent).unwrap();
    project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_node("second", json!({}))))],
        )
        .unwrap();
    assert!(matches!(
        project.redo(&agent),
        Err(ProjectRepositoryRefusal::NothingToRedo { .. })
    ));
}

#[test]
fn a_refused_operation_refuses_the_whole_transaction_and_writes_nothing() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let before = project.document_bytes();
    let commits_before = project
        .repository
        .history()
        .commits_newest_first()
        .unwrap()
        .len();
    let refusal = project
        .apply(
            &agent,
            vec![
                project.on_the_stream(add(&a_node("source", json!({})))),
                project.on_the_stream(StreamGraphOperation::RenameNode {
                    node: Uuid::new_v4(),
                    from: "absent".to_owned(),
                    to: "still-absent".to_owned(),
                }),
            ],
        )
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::GraphOperationRefused { .. }
        ),
        "{refusal}"
    );
    assert_eq!(project.document_bytes(), before);
    assert_eq!(
        project
            .repository
            .history()
            .commits_newest_first()
            .unwrap()
            .len(),
        commits_before
    );
}

#[test]
fn a_config_references_only_an_asset_the_project_holds_and_holds_it_against_deletion() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let refusal = project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_node(
                "overlay",
                json!({ "image": { "asset": Uuid::new_v4() } }),
            )))],
        )
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { .. }
        ),
        "{refusal}"
    );

    let source_directory = tempfile::tempdir().unwrap();
    let image_source = source_directory.path().join("logo.png");
    std::fs::write(&image_source, b"not really a png").unwrap();
    let imported = project
        .repository
        .apply_asset_changes(
            &agent,
            vec![ProjectAssetChangeRequest::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(image_source),
                asset_path: "media/logo.png".to_owned(),
                kind: None,
                import_settings: Map::new(),
            }],
        )
        .unwrap();
    let ProjectOperation::Asset(streamlib_project_repository::ProjectAssetOperation::ImportAsset {
        asset: image,
    }) = &imported.transaction.operations[0]
    else {
        panic!("expected an import");
    };
    let overlay = a_node("overlay", json!({ "image": { "asset": image.asset } }));
    project
        .apply(&agent, vec![project.on_the_stream(add(&overlay))])
        .unwrap();

    let refusal = project
        .repository
        .apply_asset_changes(
            &agent,
            vec![ProjectAssetChangeRequest::DeleteAsset { asset: image.asset }],
        )
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::AssetStillReferenced { .. }
        ),
        "{refusal}"
    );

    project
        .apply(
            &agent,
            vec![
                project.on_the_stream(StreamGraphOperation::RemoveNode {
                    node: overlay.clone(),
                    links: Vec::new(),
                }),
                ProjectOperation::Asset(
                    streamlib_project_repository::ProjectAssetOperation::DeleteAsset {
                        asset: image.clone(),
                    },
                ),
            ],
        )
        .unwrap();
    project.repository.asset(image.asset).unwrap_err();
    project.undo(&agent).unwrap();
    project.repository.asset(image.asset).unwrap();
    project
        .repository
        .stream_graph_document(project.stream)
        .unwrap()
        .node(overlay.id)
        .unwrap();
}
