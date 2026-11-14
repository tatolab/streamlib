// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, PROJECT_TRANSACTION_FORMAT_VERSION,
    ProjectActionApplyOutcome, ProjectActionIdempotencyKey, ProjectOperation, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionLineage, StreamGraphDocument, StreamGraphDocumentNode, StreamGraphOperation,
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
        Self::holding_the_stream(Uuid::new_v4())
    }

    fn holding_the_stream(stream: Uuid) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
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
        studio: Default::default(),
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
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();

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
            Vec::new(),
            vec![operation.clone()],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();
    let commits_after_the_first = project.repository.history().commits_newest_first().unwrap();

    let retried = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    assert_eq!(
        retried,
        ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(first)
    );
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
                Vec::new(),
                vec![operation],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap()
            .into_applied_transaction();
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

const STREAM_OF_THE_RECORD_BEFORE_FORMAT_VERSIONS: &str = "0b9f8e7d-6c5b-4a39-8281-706f5e4d3c2b";
const TRANSACTION_OF_THE_RECORD_BEFORE_FORMAT_VERSIONS: &str =
    "5a4b3c2d-1e0f-4a9b-8c7d-6e5f4a3b2c1d";

/// A trailer as a runtime before format versions recorded an `add_node` of `grade`, with
/// `{parent_commit}` standing for the commit history was at.
const TRAILER_RECORDED_BEFORE_FORMAT_VERSIONS: &str = r#"{"transaction":"5a4b3c2d-1e0f-4a9b-8c7d-6e5f4a3b2c1d","author":{"kind":"person","name":"Ada"},"parent_commit":"{parent_commit}","operations":[{"stream":"0b9f8e7d-6c5b-4a39-8281-706f5e4d3c2b","operation":"add_node","node":{"id":"6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7","name":"grade","type":"nodes.grade:Node","config":{}}}],"inverse_operations":[{"stream":"0b9f8e7d-6c5b-4a39-8281-706f5e4d3c2b","operation":"remove_node","node":{"id":"6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7","name":"grade","type":"nodes.grade:Node","config":{}}}]}"#;

/// Commit, as a runtime before format versions did, the edit the recorded trailer names: the
/// graph document written with `grade` added and the trailer as that runtime wrote it.
fn commit_the_edit_recorded_before_format_versions(project: &ProjectHoldingOneStream) {
    let stream_document_path = "streams/main.graph.json";
    let on_disk = project
        .repository
        .project_directory()
        .join(stream_document_path);
    let mut document = StreamGraphDocument::from_bytes(&std::fs::read(&on_disk).unwrap()).unwrap();
    let mut grade = a_node("grade");
    grade.id = Uuid::parse_str("6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7").unwrap();
    document
        .apply(&StreamGraphOperation::AddNode {
            node: grade,
            links: Vec::new(),
        })
        .unwrap();
    let document_bytes = document.canonical_bytes().unwrap();
    std::fs::write(&on_disk, &document_bytes).unwrap();

    let history = project.repository.history();
    let parent_commit = history.head_commit().unwrap().unwrap();
    let mut tracked_files = history.files_at_head().unwrap();
    tracked_files.insert(stream_document_path.to_owned(), document_bytes);
    let trailer =
        TRAILER_RECORDED_BEFORE_FORMAT_VERSIONS.replace("{parent_commit}", &parent_commit);
    history
        .commit_tracked_files(
            &tracked_files,
            &a_person(),
            &format!("add_node grade\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: {trailer}\n"),
        )
        .unwrap();
}

#[test]
fn a_transaction_recorded_before_format_versions_still_reads_undoes_and_redoes() {
    let mut project = ProjectHoldingOneStream::holding_the_stream(
        Uuid::parse_str(STREAM_OF_THE_RECORD_BEFORE_FORMAT_VERSIONS).unwrap(),
    );
    commit_the_edit_recorded_before_format_versions(&project);
    let recorded_before =
        Uuid::parse_str(TRANSACTION_OF_THE_RECORD_BEFORE_FORMAT_VERSIONS).unwrap();

    let read_back = &project
        .repository
        .applied_transactions_newest_first(1)
        .unwrap()[0];
    assert_eq!(read_back.transaction.transaction, recorded_before);
    assert_eq!(read_back.transaction.format_version, 1);
    assert_eq!(read_back.transaction.idempotency_key, None);
    assert_eq!(
        read_back.applied_at_unix_seconds,
        project.repository.history().commits_newest_first().unwrap()[0].committed_at_unix_seconds,
        "a record with no time of its own reads its commit's"
    );

    let undo = project.repository.undo_by(&a_person()).unwrap();
    assert_eq!(
        undo.lineage,
        ProjectTransactionLineage::UndoOf(recorded_before)
    );
    project
        .repository
        .apply_project_operations(&a_person(), None, Vec::new(), undo.operations, undo.lineage)
        .unwrap();
    assert!(project.node_names_in_the_stream().is_empty());

    let redo = project.repository.redo_by(&a_person()).unwrap();
    project
        .repository
        .apply_project_operations(&a_person(), None, Vec::new(), redo.operations, redo.lineage)
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
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();
    let history = project.repository.history();
    let mut newer = project.newest_trailer_json();
    newer["format_version"] = json!(PROJECT_TRANSACTION_FORMAT_VERSION + 1);
    newer["transaction"] = json!(Uuid::new_v4());
    newer["operations"] = json!([
        { "operation": "an_operation_this_runtime_does_not_know", "path": "nodes/grade.py" }
    ]);
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
    let operation = project.add_node_operation(&a_node("grade"));
    let applied = project
        .repository
        .apply_project_operations(
            &a_person(),
            None,
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();
    let commit = &project.repository.history().commits_newest_first().unwrap()[0];
    assert_eq!(commit.commit, applied.commit);
    assert_eq!(
        applied.applied_at_unix_seconds,
        commit.committed_at_unix_seconds
    );
    assert_eq!(
        project
            .repository
            .applied_transactions_newest_first(1)
            .unwrap()[0]
            .applied_at_unix_seconds,
        applied.applied_at_unix_seconds
    );
}

#[test]
fn a_key_recorded_before_the_project_was_reopened_still_answers_its_transaction() {
    let mut project = ProjectHoldingOneStream::new();
    let idempotency_key =
        ProjectActionIdempotencyKey::from_caller("gesture-before-restart").unwrap();
    let operation = project.add_node_operation(&a_node("grade"));
    let first = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            Vec::new(),
            vec![operation.clone()],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();

    project.repository = ProjectRepository::open(project.repository.project_directory()).unwrap();
    let retried = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    assert_eq!(
        retried,
        ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(first)
    );
    assert_eq!(project.node_names_in_the_stream(), vec!["grade"]);
}

#[test]
fn a_trailer_with_no_readable_envelope_leaves_the_project_openable() {
    let project = ProjectHoldingOneStream::new();
    let history = project.repository.history();
    history
        .commit_tracked_files(
            &history.files_at_head().unwrap(),
            &a_person(),
            &format!("unreadable\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: not json\n"),
        )
        .unwrap();

    ProjectRepository::open(project.repository.project_directory())
        .expect("one unreadable trailer does not keep the project from opening");
}

#[test]
fn a_key_sent_again_answers_its_transaction_past_a_newer_unreadable_trailer() {
    let mut project = ProjectHoldingOneStream::new();
    let idempotency_key =
        ProjectActionIdempotencyKey::from_caller("gesture-before-an-unreadable-commit").unwrap();
    let operation = project.add_node_operation(&a_node("grade"));
    let first = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            Vec::new(),
            vec![operation.clone()],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap()
        .into_applied_transaction();
    let history = project.repository.history();
    history
        .commit_tracked_files(
            &history.files_at_head().unwrap(),
            &a_person(),
            &format!("unreadable\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: not json\n"),
        )
        .unwrap();

    project.repository = ProjectRepository::open(project.repository.project_directory()).unwrap();
    let retried = project
        .repository
        .apply_project_operations(
            &a_person(),
            Some(&idempotency_key),
            Vec::new(),
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    assert_eq!(
        retried,
        ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(first)
    );
}
