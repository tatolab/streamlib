// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, json};
use streamlib_project_repository::{
    ProjectAssetAction, ProjectAssetImportSource, ProjectOperation, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionLineage, ProjectTransactionUndoState, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphDocumentNode, StreamGraphFragmentPlacement,
    StreamGraphNodeStudioFields, StreamGraphOperation, StreamGraphOperationOnAStream,
    fragment_with_each_node_uuid_kept_by_its_name,
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
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                operations,
                ProjectTransactionLineage::NewEdit,
            )
            .map(|_| ())
    }

    fn undo(&mut self, author: &ProjectTransactionAuthor) -> Result<(), ProjectRepositoryRefusal> {
        let reversal = self.repository.undo_by(author)?;
        self.repository
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .map(|_| ())
    }

    fn redo(&mut self, author: &ProjectTransactionAuthor) -> Result<(), ProjectRepositoryRefusal> {
        let reversal = self.repository.redo_by(author)?;
        self.repository
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
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
        exposed_config_fields: Vec::new(),
        studio: Default::default(),
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
                    from: Default::default(),
                    to: Box::new(
                        StreamGraphNodeStudioFields::try_from(
                            json!({"position": [120, 40]}).as_object().unwrap().clone(),
                        )
                        .unwrap(),
                    ),
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
        document.node(meter.id).unwrap().studio.to_map()["position"],
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
fn each_edit_and_redo_carries_where_it_stands_with_its_authors_undo_and_redo() {
    let mut project = ProjectHoldingOneStream::new();
    let person = an_author(ProjectTransactionAuthorKind::Person, "Jo");
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    for name in ["kept", "undone"] {
        project
            .apply(
                &person,
                vec![project.on_the_stream(add(&a_node(name, json!({}))))],
            )
            .unwrap();
    }
    project.undo(&person).unwrap();
    project.redo(&person).unwrap();
    project.undo(&person).unwrap();
    project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_node("replaced", json!({}))))],
        )
        .unwrap();
    project.undo(&agent).unwrap();
    project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_node("replacement", json!({}))))],
        )
        .unwrap();

    let mut oldest_first = project
        .repository
        .applied_transactions_newest_first(usize::MAX)
        .unwrap();
    oldest_first.reverse();
    let undo_state_of_each_transaction = project
        .repository
        .the_undo_state_of_each_transaction()
        .unwrap();
    let undo_states_oldest_first: Vec<Option<ProjectTransactionUndoState>> = oldest_first
        .iter()
        .map(|applied| {
            undo_state_of_each_transaction
                .get(&applied.transaction.transaction)
                .copied()
        })
        .collect();

    assert_eq!(
        undo_states_oldest_first,
        vec![
            Some(ProjectTransactionUndoState::Applied),
            Some(ProjectTransactionUndoState::UndoneRedoable),
            None,
            Some(ProjectTransactionUndoState::UndoneRedoable),
            None,
            Some(ProjectTransactionUndoState::UndoneNotRedoable),
            None,
            Some(ProjectTransactionUndoState::Applied),
        ]
    );
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
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(image_source),
                asset_path: Some("media/logo.png".to_owned()),
                file_name: None,
                kind: None,
                import_settings: Map::new(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
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
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: image.asset }],
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

fn a_look_fragment_whose_inner_grade_reads(
    lut: Uuid,
    previous_run: Option<&serde_json::Value>,
) -> serde_json::Value {
    fragment_with_each_node_uuid_kept_by_its_name(
        &json!({
            "nodes": [
                { "name": "blur", "type": "nodes.blur:Node", "config": {} },
                {
                    "name": "inner",
                    "type": "looks.inner:inner",
                    "config": {},
                    "fragment": {
                        "nodes": [{
                            "name": "grade",
                            "type": "nodes.grade:Node",
                            "config": { "lut": { "asset": lut } }
                        }],
                        "links": []
                    }
                }
            ],
            "links": []
        }),
        previous_run,
        &mut Uuid::new_v4,
    )
    .unwrap()
}

fn a_look_fragment_node(fragment: serde_json::Value) -> StreamGraphDocumentNode {
    StreamGraphDocumentNode {
        node_type: "looks.look:look".to_owned(),
        fragment: Some(Box::new(fragment)),
        ..a_node("look", json!({}))
    }
}

fn an_imported_asset(
    project: &mut ProjectHoldingOneStream,
    author: &ProjectTransactionAuthor,
) -> Uuid {
    let source_directory = tempfile::tempdir().unwrap();
    let source = source_directory.path().join("grade.cube");
    std::fs::write(&source, b"not really a lut").unwrap();
    let imported = project
        .repository
        .apply_asset_changes(
            author,
            None,
            vec![ProjectAssetAction::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(source),
                asset_path: Some("media/grade.cube".to_owned()),
                file_name: None,
                kind: None,
                import_settings: Map::new(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    let ProjectOperation::Asset(streamlib_project_repository::ProjectAssetOperation::ImportAsset {
        asset,
    }) = &imported.transaction.operations[0]
    else {
        panic!("expected an import");
    };
    asset.asset
}

#[test]
fn a_fragment_placed_or_refreshed_with_an_inner_node_naming_an_absent_asset_is_refused() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let absent_lut = Uuid::new_v4();
    let refusal = project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_look_fragment_node(
                a_look_fragment_whose_inner_grade_reads(absent_lut, None),
            )))],
        )
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { asset, .. } if asset == absent_lut
        ),
        "{refusal}"
    );

    let held_lut = an_imported_asset(&mut project, &agent);
    let first_run = a_look_fragment_whose_inner_grade_reads(held_lut, None);
    let look = a_look_fragment_node(first_run.clone());
    project
        .apply(&agent, vec![project.on_the_stream(add(&look))])
        .unwrap();
    let before = project.document_bytes();
    let refusal = project
        .apply(
            &agent,
            vec![
                project.on_the_stream(StreamGraphOperation::RefreshFragment {
                    node: look.id,
                    from: StreamGraphFragmentPlacement {
                        generator: look.node_type.clone(),
                        config: json!({}),
                        fragment: first_run.clone(),
                    },
                    to: StreamGraphFragmentPlacement {
                        generator: look.node_type.clone(),
                        config: json!({}),
                        fragment: a_look_fragment_whose_inner_grade_reads(
                            absent_lut,
                            Some(&first_run),
                        ),
                    },
                }),
            ],
        )
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { asset, .. } if asset == absent_lut
        ),
        "{refusal}"
    );
    assert_eq!(project.document_bytes(), before);
}

#[test]
fn an_asset_only_a_fragments_inner_node_references_is_held_against_deletion() {
    let mut project = ProjectHoldingOneStream::new();
    let agent = an_author(ProjectTransactionAuthorKind::Agent, "rig agent");
    let lut = an_imported_asset(&mut project, &agent);
    project
        .apply(
            &agent,
            vec![project.on_the_stream(add(&a_look_fragment_node(
                a_look_fragment_whose_inner_grade_reads(lut, None),
            )))],
        )
        .unwrap();

    let refusal = project
        .repository
        .apply_asset_changes(
            &agent,
            None,
            vec![ProjectAssetAction::DeleteAsset { asset: lut }],
        )
        .unwrap_err();
    assert!(
        matches!(
            &refusal,
            ProjectRepositoryRefusal::AssetStillReferenced { asset, referencing_documents }
                if *asset == lut && referencing_documents == &["streams/main.graph.json"]
        ),
        "{refusal}"
    );
    project.repository.asset(lut).unwrap();
}
