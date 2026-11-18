// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project whose history an older runtime recorded, in format 2, with `set_pin_value` and
//! `set_node_config` edits: it opens, takes an edit, and undoes and redoes back through the old
//! ones, each read as the `set_node_input_value` operations this runtime records.
//!
//! The fixture is a recording, never edited by hand: each commit's message and files as the
//! project repository at the commit it names wrote them, through its own `set_pin_value_of` and
//! `set_node_config_of`, with the actions as the API server sent them.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectAction, ProjectHistoryRepository, ProjectOperation,
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionUndoState, StreamGraphAction,
    StreamGraphOperation, StreamGraphOperationOnAStream,
};
use uuid::Uuid;

/// The recorded history: each commit, oldest first, with its message and every file it holds.
const HISTORY_RECORDED_IN_FORMAT_2: &str =
    include_str!("fixtures/history/set_pin_value_and_set_node_config_recorded_in_format_2.json");

/// A history recorded the same way: a `set_node_config` of `{}` on `passthrough`, a node whose
/// type takes no config and which holds no value.
const AN_EMPTY_SET_NODE_CONFIG_RECORDED_IN_FORMAT_2: &str =
    include_str!("fixtures/history/an_empty_set_node_config_recorded_in_format_2.json");

const STREAM: &str = "860fdbd6-8fe2-592b-8ebc-faae672abefa";
const MIXER: &str = "6b6b6b6b-6b6b-4b6b-8b6b-6b6b6b6b6b6b";

/// A project directory under `root` whose history holds the commits `recording` records, in
/// order, and whose files are the newest commit's.
fn the_recorded_project_under(root: &std::path::Path, recording: &str) -> ProjectRepository {
    let recorded: Value = serde_json::from_str(recording).unwrap();
    let project_directory = root.join("rig");
    std::fs::create_dir_all(&project_directory).unwrap();
    let history = ProjectHistoryRepository::create_for_project(&project_directory).unwrap();
    let trailer_prefix = format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: ");
    let mut files_at_head = BTreeMap::new();
    for commit in recorded["commits"].as_array().unwrap() {
        let message = commit["message"].as_str().unwrap();
        let author = message
            .lines()
            .find_map(|line| line.strip_prefix(&trailer_prefix))
            .map(|trailer| {
                let trailer: Value = serde_json::from_str(trailer).unwrap();
                serde_json::from_value(trailer["author"].clone()).unwrap()
            })
            .unwrap_or_else(ProjectTransactionAuthor::the_runtime);
        files_at_head = commit["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, text)| (path.clone(), text.as_str().unwrap().as_bytes().to_vec()))
            .collect::<BTreeMap<String, Vec<u8>>>();
        history
            .commit_tracked_files(&files_at_head, &author, message)
            .unwrap();
    }
    for (path, bytes) in &files_at_head {
        let on_disk = project_directory.join(path);
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(on_disk, bytes).unwrap();
    }
    ProjectRepository::open(&project_directory).unwrap()
}

fn alice() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Agent,
        name: "alice".to_owned(),
    }
}

fn stream() -> Uuid {
    Uuid::parse_str(STREAM).unwrap()
}

fn mixer() -> Uuid {
    Uuid::parse_str(MIXER).unwrap()
}

fn the_input_values_of_the_mixer(project: &ProjectRepository) -> Value {
    Value::Object(
        project
            .stream_graph_document(stream())
            .unwrap()
            .node(mixer())
            .unwrap()
            .input_values,
    )
}

fn set_of_the_mixer(input: &str, from: Option<Value>, to: Option<Value>) -> ProjectOperation {
    ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
        stream: stream(),
        graph_operation: StreamGraphOperation::SetNodeInputValue {
            node: mixer(),
            input: input.to_owned(),
            from,
            to,
        },
    })
}

/// Undo `alice`'s latest edit; the operations the undo applied.
fn undo(project: &mut ProjectRepository) -> Vec<ProjectOperation> {
    let undo = project.undo_by(&alice()).unwrap();
    project
        .apply_project_operations(
            &alice(),
            None,
            Vec::new(),
            undo.operations.clone(),
            undo.lineage,
        )
        .unwrap();
    undo.operations
}

/// Redo `alice`'s latest undo; the operations the redo applied.
fn redo(project: &mut ProjectRepository) -> Vec<ProjectOperation> {
    let redo = project.redo_by(&alice()).unwrap();
    project
        .apply_project_operations(
            &alice(),
            None,
            Vec::new(),
            redo.operations.clone(),
            redo.lineage,
        )
        .unwrap();
    redo.operations
}

/// Whether each of `operations` sets one input value.
fn each_sets_one_input_value(operations: &[ProjectOperation]) -> bool {
    !operations.is_empty()
        && operations.iter().all(|operation| {
            matches!(
                operation,
                ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                    graph_operation: StreamGraphOperation::SetNodeInputValue { .. },
                    ..
                })
            )
        })
}

#[test]
fn each_recorded_edit_is_listed_as_the_input_value_sets_it_stands_for() {
    let root = tempfile::tempdir().unwrap();
    let project = the_recorded_project_under(root.path(), HISTORY_RECORDED_IN_FORMAT_2);

    let mut listed = project
        .applied_transactions_newest_first(usize::MAX)
        .unwrap();
    listed.reverse();
    assert_eq!(listed.len(), 3, "the three recorded edits");
    assert!(
        listed
            .iter()
            .all(|applied| applied.transaction.format_version == 2),
        "each keeps the format it was recorded in"
    );
    let operations: Vec<Vec<ProjectOperation>> = listed
        .iter()
        .map(|applied| applied.transaction.operations.clone())
        .collect();
    assert_eq!(
        operations,
        vec![
            vec![set_of_the_mixer("trim", Some(json!(0.5)), None)],
            vec![
                set_of_the_mixer("frame_width", Some(json!(640)), Some(json!(1280))),
                set_of_the_mixer("gain", Some(json!(0.25)), Some(json!(0.75))),
            ],
            vec![set_of_the_mixer(
                "gain",
                Some(json!(0.75)),
                Some(json!(0.9))
            )],
        ]
    );
    assert_eq!(
        listed[1].transaction.inverse_operations,
        vec![
            set_of_the_mixer("frame_width", Some(json!(1280)), Some(json!(640))),
            set_of_the_mixer("gain", Some(json!(0.75)), Some(json!(0.25))),
        ]
    );

    assert_eq!(
        listed[2].transaction.actions,
        vec![ProjectAction::StreamGraph(
            StreamGraphAction::SetNodeInputValue {
                stream: stream(),
                level: Vec::new(),
                node: MIXER.to_owned(),
                input: "gain".to_owned(),
                value: Some(json!(0.9)),
            }
        )],
        "a `set_pin_value` sent reads as the `set_node_input_value` it stands for"
    );
    assert_eq!(
        listed[1].transaction.actions,
        vec![ProjectAction::StreamGraph(
            StreamGraphAction::SetNodeConfig {
                stream: stream(),
                level: Vec::new(),
                node: MIXER.to_owned(),
                config: Map::from_iter([
                    ("frame_width".to_owned(), json!(1280)),
                    ("gain".to_owned(), json!(0.75)),
                ]),
            }
        )],
        "a `set_node_config` sent names no value it replaced, so it stays as sent"
    );
}

#[test]
fn a_recorded_project_takes_an_edit_then_undoes_and_redoes_back_through_the_old_ones() {
    let root = tempfile::tempdir().unwrap();
    let mut project = the_recorded_project_under(root.path(), HISTORY_RECORDED_IN_FORMAT_2);
    assert_eq!(
        the_input_values_of_the_mixer(&project),
        json!({ "gain": 0.9, "frame_width": 1280 })
    );

    project
        .apply_project_operations(
            &alice(),
            None,
            Vec::new(),
            vec![set_of_the_mixer("gain", Some(json!(0.9)), Some(json!(1.0)))],
            streamlib_project_repository::ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    assert_eq!(
        the_input_values_of_the_mixer(&project),
        json!({ "gain": 1.0, "frame_width": 1280 })
    );

    let each_state_undone_to = [
        json!({ "gain": 0.9, "frame_width": 1280 }),
        json!({ "gain": 0.75, "frame_width": 1280 }),
        json!({ "gain": 0.25, "frame_width": 640 }),
        json!({ "gain": 0.25, "frame_width": 640, "trim": 0.5 }),
    ];
    for undone_to in &each_state_undone_to {
        let undone = undo(&mut project);
        assert!(each_sets_one_input_value(&undone), "{undone:?}");
        assert_eq!(&the_input_values_of_the_mixer(&project), undone_to);
    }
    assert!(matches!(
        project.undo_by(&alice()),
        Err(ProjectRepositoryRefusal::NothingToUndo { .. })
    ));
    assert!(
        project
            .the_undo_state_of_each_transaction()
            .unwrap()
            .values()
            .all(|state| *state == ProjectTransactionUndoState::UndoneRedoable)
    );

    for redone_to in each_state_undone_to.iter().rev().skip(1) {
        let redone = redo(&mut project);
        assert!(each_sets_one_input_value(&redone), "{redone:?}");
        assert_eq!(&the_input_values_of_the_mixer(&project), redone_to);
    }
    redo(&mut project);
    assert_eq!(
        the_input_values_of_the_mixer(&project),
        json!({ "gain": 1.0, "frame_width": 1280 })
    );
}

/// A recorded `set_node_config` that names no value stays the edit it was recorded as, so it
/// undoes and redoes as a change of nothing rather than as an edit with no operation.
#[test]
fn a_recorded_set_node_config_naming_no_value_undoes_and_redoes() {
    let root = tempfile::tempdir().unwrap();
    let mut project =
        the_recorded_project_under(root.path(), AN_EMPTY_SET_NODE_CONFIG_RECORDED_IN_FORMAT_2);
    let recorded_edit = &project.applied_transactions_newest_first(1).unwrap()[0];
    assert!(
        matches!(
            recorded_edit.transaction.operations.as_slice(),
            [ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    graph_operation: StreamGraphOperation::SetNodeConfig { .. },
                    ..
                }
            )]
        ),
        "{:?}",
        recorded_edit.transaction.operations
    );
    let document_before = project
        .stream_graph_document(recorded_stream_of(&project))
        .unwrap()
        .to_value();

    assert_eq!(undo(&mut project).len(), 1);
    assert_eq!(redo(&mut project).len(), 1);
    assert_eq!(
        project
            .stream_graph_document(recorded_stream_of(&project))
            .unwrap()
            .to_value(),
        document_before
    );
    assert_eq!(undo(&mut project).len(), 1, "the redo undoes in turn");
}

/// The stream the newest recorded edit changed.
fn recorded_stream_of(project: &ProjectRepository) -> Uuid {
    let newest = &project.applied_transactions_newest_first(1).unwrap()[0];
    match &newest.transaction.operations[0] {
        ProjectOperation::StreamGraph(StreamGraphOperationOnAStream { stream, .. }) => *stream,
        operation => panic!("no graph edit: {operation:?}"),
    }
}
