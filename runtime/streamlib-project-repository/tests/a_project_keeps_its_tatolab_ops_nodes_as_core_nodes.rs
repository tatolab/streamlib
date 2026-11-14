// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib_project_repository::{
    ProjectAssetAction, ProjectOperation, ProjectRepository, ProjectStreamDocumentOperation,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, ProjectTransactionLineage,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphOperation, StreamGraphOperationOnAStream, TATOLAB_OPS_NODES_MOVED_TO_CORE_NODES_KEY,
};
use uuid::Uuid;

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Ada".to_owned(),
    }
}

fn a_node(id: u128, name: &str, node_type: &str) -> StreamGraphDocumentNode {
    serde_json::from_value(json!({
        "id": Uuid::from_u128(id),
        "name": name,
        "type": node_type,
        "config": {},
    }))
    .unwrap()
}

/// A project placed with an earlier runtime: an Lfo of `tatolab-ops` driving a Switch of it, and
/// a Feedback, beside a node of the project's own.
struct AProjectPlacedWithTatolabOps {
    _root: tempfile::TempDir,
    repository: ProjectRepository,
    stream: Uuid,
}

impl AProjectPlacedWithTatolabOps {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut repository = ProjectRepository::create(
            &root.path().join("show"),
            "show",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let created = repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::CreateStream {
                    path: "streams/main.graph.json".to_owned(),
                    display_name: "main".to_owned(),
                }],
            )
            .unwrap()
            .into_applied_transaction();
        let stream = match &created.transaction.operations[..] {
            [
                ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::CreateStreamDocument { stream, .. },
                ),
            ] => *stream,
            other => panic!("a create records one created document, not {other:?}"),
        };
        let on_the_stream = |graph_operation| {
            ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            })
        };
        let lfo_to_switch = StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: Uuid::from_u128(1),
                port: "result".to_owned(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: Uuid::from_u128(2),
                port: "when_on".to_owned(),
            },
        };
        repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                vec![
                    on_the_stream(StreamGraphOperation::AddNode {
                        node: a_node(1, "lfo", "tatolab.ops:Lfo"),
                        links: Vec::new(),
                    }),
                    on_the_stream(StreamGraphOperation::AddNode {
                        node: a_node(2, "switch", "tatolab.ops:Switch"),
                        links: vec![lfo_to_switch],
                    }),
                    on_the_stream(StreamGraphOperation::AddNode {
                        node: a_node(3, "feedback", "tatolab.ops:Feedback"),
                        links: Vec::new(),
                    }),
                    on_the_stream(StreamGraphOperation::AddNode {
                        node: a_node(4, "tone", "nodes.tone:Tone"),
                        links: Vec::new(),
                    }),
                ],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap();
        Self {
            _root: root,
            repository,
            stream,
        }
    }

    fn type_of(&self, name: &str) -> String {
        ProjectRepository::open(self.repository.project_directory())
            .unwrap()
            .stream_graph_document(self.stream)
            .unwrap()
            .node_named(name)
            .unwrap()
            .node_type
    }

    fn links(&self) -> Vec<StreamGraphDocumentLink> {
        self.repository
            .stream_graph_document(self.stream)
            .unwrap()
            .links()
    }
}

#[test]
fn each_tatolab_ops_node_is_moved_to_its_core_nodes_type_by_the_runtime_in_one_transaction() {
    let mut project = AProjectPlacedWithTatolabOps::new();
    let links_before = project.links();

    let moved = project
        .repository
        .move_each_tatolab_ops_node_to_core_nodes()
        .unwrap()
        .expect("a project holding tatolab-ops nodes is moved")
        .into_applied_transaction();

    assert_eq!(
        moved.transaction.author,
        ProjectTransactionAuthor::the_runtime()
    );
    assert_eq!(
        moved
            .transaction
            .idempotency_key
            .as_ref()
            .map(|key| key.as_str()),
        Some(TATOLAB_OPS_NODES_MOVED_TO_CORE_NODES_KEY)
    );
    assert_eq!(
        project.type_of("lfo"),
        "imports/core-nodes/core_nodes.animation:Lfo"
    );
    assert_eq!(
        project.type_of("switch"),
        "imports/core-nodes/core_nodes.logic:Switch"
    );
    assert_eq!(project.type_of("feedback"), "tatolab.stream:PreviousValue");
    assert_eq!(project.type_of("tone"), "nodes.tone:Tone");
    assert_eq!(project.links(), links_before);
}

#[test]
fn a_project_already_moved_is_not_moved_again_even_once_the_move_is_undone() {
    let mut project = AProjectPlacedWithTatolabOps::new();
    project
        .repository
        .move_each_tatolab_ops_node_to_core_nodes()
        .unwrap()
        .expect("the first move");
    let reversal = project
        .repository
        .undo_by(&ProjectTransactionAuthor::the_runtime())
        .unwrap();
    project
        .repository
        .apply_project_operations(
            &ProjectTransactionAuthor::the_runtime(),
            None,
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        )
        .unwrap();
    assert_eq!(project.type_of("lfo"), "tatolab.ops:Lfo");

    assert!(
        project
            .repository
            .move_each_tatolab_ops_node_to_core_nodes()
            .unwrap()
            .is_none()
    );
    assert!(
        ProjectRepository::open(project.repository.project_directory())
            .unwrap()
            .move_each_tatolab_ops_node_to_core_nodes()
            .unwrap()
            .is_none()
    );
    assert_eq!(project.type_of("lfo"), "tatolab.ops:Lfo");
}

#[test]
fn a_project_holding_no_tatolab_ops_node_records_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut repository = ProjectRepository::create(
        &root.path().join("quiet"),
        "quiet",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    repository
        .apply_asset_changes(
            &a_person(),
            None,
            vec![ProjectAssetAction::CreateStream {
                path: "streams/main.graph.json".to_owned(),
                display_name: "main".to_owned(),
            }],
        )
        .unwrap();
    assert!(
        repository
            .move_each_tatolab_ops_node_to_core_nodes()
            .unwrap()
            .is_none()
    );
}
