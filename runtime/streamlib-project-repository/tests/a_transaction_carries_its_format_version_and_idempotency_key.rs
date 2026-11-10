// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, PROJECT_TRANSACTION_FORMAT_VERSION,
    ProjectActionIdempotencyKey, ProjectOperation, ProjectRepository, ProjectRepositoryRefusal,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, ProjectTransactionLineage,
    StreamGraphDocument, StreamGraphDocumentNode, StreamGraphOperation,
    StreamGraphOperationOnAStream,
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

    fn add_node_operation(&self, node: &StreamGraphDocumentNode) -> ProjectOperation {
        ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
            stream: self.stream,
            graph_operation: StreamGraphOperation::AddNode {
                node: node.clone(),
                links: Vec::new(),
            },
        })
    }

    fn node_names_in_the_stream(&self) -> Vec<String> {
        self.repository
            .stream_graph_document(self.stream)
            .unwrap()
            .to_value()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn newest_trailer_json(&self) -> Value {
        let trailer_prefix = format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: ");
        let newest = &self.repository.history().commits_newest_first().unwrap()[0];
        let trailer = newest
            .message
            .lines()
            .find_map(|line| line.strip_prefix(&trailer_prefix))
            .unwrap();
        serde_json::from_str(trailer).unwrap()
    }
}

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "Ada".to_owned(),
    }
}

fn a_node(name: &str) -> StreamGraphDocumentNode {
    StreamGraphDocumentNode {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        node_type: format!("nodes.{name}:Node"),
        config: json!({}),
        pins: Map::new(),
        exposed_config_fields: Vec::new(),
        studio: Map::new(),
        fragment: None,
    }
}

#[test]
fn a_new_transaction_records_the_format_version_and_the_callers_idempotency_key() {
    let mut project = ProjectHoldingOneStream::new();
    let idempotency_key = ProjectActionIdempotencyKey::from_caller("drag-7f3a").unwrap();
    let operation = project.add_node_operation(&a_node("grade"));
    let applied = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    let trailer = project.newest_trailer_json();
    assert_eq!(
        trailer["format_version"],
        json!(PROJECT_TRANSACTION_FORMAT_VERSION)
    );
    assert_eq!(trailer["idempotency_key"], json!("drag-7f3a"));
    assert_eq!(
        applied.transaction.idempotency_key.as_ref(),
        Some(&idempotency_key)
    );
    assert_eq!(
        project
            .repository
            .applied_transactions_newest_first(1)
            .unwrap()[0],
        applied
    );
}

#[test]
fn an_action_sent_again_under_its_key_applies_once_and_answers_the_first_transaction() {
    let mut project = ProjectHoldingOneStream::new();
    let idempotency_key = ProjectActionIdempotencyKey::from_caller("gesture-1").unwrap();
    let operation = project.add_node_operation(&a_node("grade"));
    let first = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            vec![operation.clone()],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    let commits_after_the_first = project.repository.history().commits_newest_first().unwrap();

    let retried = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    assert_eq!(retried, first);
    assert_eq!(
        project.repository.history().commits_newest_first().unwrap(),
        commits_after_the_first
    );
    assert_eq!(project.node_names_in_the_stream(), vec!["grade"]);
}

#[test]
fn an_action_sent_without_a_key_applies_each_time() {
    let mut project = ProjectHoldingOneStream::new();
    for name in ["grade", "blur"] {
        let operation = project.add_node_operation(&a_node(name));
        project
            .repository
            .apply_project_operations(
                &a_person(),
                None,
                vec![operation],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap();
    }
    assert_eq!(project.node_names_in_the_stream(), vec!["grade", "blur"]);
    assert!(
        project
            .newest_trailer_json()
            .get("idempotency_key")
            .is_none()
    );
}

#[test]
fn a_blank_idempotency_key_is_refused() {
    assert!(matches!(
        ProjectActionIdempotencyKey::from_caller("  "),
        Err(ProjectRepositoryRefusal::BlankIdempotencyKey)
    ));
}

/// Commit, as a runtime before format versions did, an edit adding `node`: the graph document
/// written with it and a trailer in that runtime's envelope, which named no format version and
/// no idempotency key.
fn commit_an_edit_in_the_format_before_versions(
    project: &ProjectHoldingOneStream,
    node: &StreamGraphDocumentNode,
) -> Uuid {
    let stream_document_path = "streams/main.graph.json";
    let on_disk = project
        .repository
        .project_directory()
        .join(stream_document_path);
    let mut document = StreamGraphDocument::from_bytes(&std::fs::read(&on_disk).unwrap()).unwrap();
    let add_node = StreamGraphOperation::AddNode {
        node: node.clone(),
        links: Vec::new(),
    };
    document.apply(&add_node).unwrap();
    let document_bytes = document.canonical_bytes().unwrap();
    std::fs::write(&on_disk, &document_bytes).unwrap();

    let history = project.repository.history();
    let parent_commit = history.head_commit().unwrap().unwrap();
    let mut tracked_files = history.files_at_head().unwrap();
    tracked_files.insert(stream_document_path.to_owned(), document_bytes);
    let operation = project.add_node_operation(node);
    let inverse_operation = operation.inverse_at(&parent_commit);
    let transaction = Uuid::new_v4();
    let envelope_before_format_versions = json!({
        "transaction": transaction,
        "author": { "kind": "person", "name": "Ada" },
        "parent_commit": parent_commit,
        "operations": [operation],
        "inverse_operations": [inverse_operation],
    });
    history
        .commit_tracked_files(
            &tracked_files,
            &a_person(),
            &format!(
                "add_node {}\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: {envelope_before_format_versions}\n",
                node.name
            ),
        )
        .unwrap();
    transaction
}

#[test]
fn a_transaction_recorded_before_format_versions_still_reads_undoes_and_redoes() {
    let mut project = ProjectHoldingOneStream::new();
    let recorded_before = commit_an_edit_in_the_format_before_versions(&project, &a_node("grade"));

    let read_back = &project
        .repository
        .applied_transactions_newest_first(1)
        .unwrap()[0];
    assert_eq!(read_back.transaction.transaction, recorded_before);
    assert_eq!(read_back.transaction.format_version, 1);
    assert_eq!(read_back.transaction.idempotency_key, None);

    let undo = project.repository.undo_by(&a_person()).unwrap();
    assert_eq!(
        undo.lineage,
        ProjectTransactionLineage::UndoOf(recorded_before)
    );
    project
        .repository
        .apply_project_operations(&a_person(), None, undo.operations, undo.lineage)
        .unwrap();
    assert!(project.node_names_in_the_stream().is_empty());

    let redo = project.repository.redo_by(&a_person()).unwrap();
    project
        .repository
        .apply_project_operations(&a_person(), None, redo.operations, redo.lineage)
        .unwrap();
    assert_eq!(project.node_names_in_the_stream(), vec!["grade"]);
}

#[test]
fn a_transaction_in_a_format_newer_than_this_runtime_is_refused_by_name() {
    let mut project = ProjectHoldingOneStream::new();
    let operation = project.add_node_operation(&a_node("grade"));
    project
        .repository
        .apply_project_operations(
            &a_person(),
            None,
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    let history = project.repository.history();
    let mut newer = project.newest_trailer_json();
    newer["format_version"] = json!(PROJECT_TRANSACTION_FORMAT_VERSION + 1);
    newer["transaction"] = json!(Uuid::new_v4());
    history
        .commit_tracked_files(
            &history.files_at_head().unwrap(),
            &a_person(),
            &format!("add_node grade\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: {newer}\n"),
        )
        .unwrap();

    assert!(matches!(
        project.repository.applied_transactions_newest_first(1),
        Err(ProjectRepositoryRefusal::TransactionFormatNewerThanThisRuntime {
            format_version,
            newest_format_version: PROJECT_TRANSACTION_FORMAT_VERSION,
            ..
        }) if format_version == PROJECT_TRANSACTION_FORMAT_VERSION + 1
    ));
}

#[test]
fn an_applied_transaction_carries_when_its_commit_was_made() {
    let mut project = ProjectHoldingOneStream::new();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let operation = project.add_node_operation(&a_node("grade"));
    let applied = project
        .repository
        .apply_project_operations(
            &a_person(),
            None,
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((before..=after).contains(&applied.applied_at_unix_seconds));
    assert_eq!(
        project
            .repository
            .applied_transactions_newest_first(1)
            .unwrap()[0]
            .applied_at_unix_seconds,
        applied.applied_at_unix_seconds
    );
}
