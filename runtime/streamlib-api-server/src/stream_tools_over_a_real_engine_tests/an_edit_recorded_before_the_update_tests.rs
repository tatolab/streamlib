// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project whose history an older runtime recorded, with `set_pin_value` and
//! `set_node_config` edits on a running stream: undo reaches back through them, each read as
//! the input value sets it stands for, and stops, in the runtime's words, at an edit whose input
//! the node no longer has.

use std::collections::BTreeMap;

use streamlib_project_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryRepository, ProjectTransactionAuthor,
};

use super::input_value_set_tests::{
    InputValueTestNode, MIXER_NODE_UUID, register_the_input_value_test_node_once,
    the_input_values_of,
};
use super::*;

/// The history the project repository recorded, in format 2, at the commit it names: `mixer`,
/// an [`InputValueTestNode`] by [`MIXER_NODE_UUID`], had its `trim` taken off by
/// `set_pin_value`, its `frame_width` and `gain` set by `set_node_config`, then its `gain` set
/// by `set_pin_value`. Each commit's message and files, oldest first, as that runtime wrote
/// them; never edited by hand.
const HISTORY_RECORDED_IN_FORMAT_2: &str = include_str!(
    "../../../streamlib-project-repository/tests/fixtures/history/set_pin_value_and_set_node_config_recorded_in_format_2.json"
);

impl ProjectWithGraphDocuments {
    /// The project [`HISTORY_RECORDED_IN_FORMAT_2`] records, with a venv: its history holds
    /// each recorded commit, in order, and its files are the newest commit's.
    fn holding_the_history_recorded_in_format_2() -> Self {
        let recorded: Value =
            serde_json::from_str(HISTORY_RECORDED_IN_FORMAT_2).expect("the recorded history reads");
        let commits = recorded["commits"]
            .as_array()
            .expect("the recording lists its commits");
        let files_of = |commit: &Value| -> BTreeMap<String, Vec<u8>> {
            commit["files"]
                .as_object()
                .expect("a recorded commit lists its files")
                .iter()
                .map(|(path, text)| {
                    let text = text.as_str().expect("a recorded file is text");
                    (path.clone(), text.as_bytes().to_vec())
                })
                .collect()
        };
        let files_at_head = files_of(commits.last().expect("the recording holds a commit"));
        let project_file: Value = serde_json::from_slice(&files_at_head["tatolab-project.json"])
            .expect("the recorded project file reads");
        let mut project = Self::with_no_venv();
        project.project = project_file["project"]
            .as_str()
            .and_then(|project| Uuid::parse_str(project).ok())
            .expect("the recorded project file names its project");

        let history = ProjectHistoryRepository::create_for_project(project.path())
            .expect("the project's history");
        let trailer_prefix = format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: ");
        for commit in commits {
            let message = commit["message"]
                .as_str()
                .expect("a recorded commit has its message");
            let author = message
                .lines()
                .find_map(|line| line.strip_prefix(&trailer_prefix))
                .map(|trailer| {
                    let trailer: Value =
                        serde_json::from_str(trailer).expect("a recorded trailer reads");
                    serde_json::from_value(trailer["author"].clone())
                        .expect("a recorded trailer names its author")
                })
                .unwrap_or_else(ProjectTransactionAuthor::the_runtime);
            history
                .commit_tracked_files(&files_of(commit), &author, message)
                .expect("the recorded commit is committed again");
        }
        for (path, bytes) in &files_at_head {
            let on_disk = project.path().join(path);
            std::fs::create_dir_all(on_disk.parent().expect("a recorded file's folder"))
                .expect("a recorded file's folder");
            std::fs::write(on_disk, bytes).expect("a recorded file");
        }
        std::fs::create_dir_all(project.interpreter().parent().expect("the venv's bin"))
            .expect("the venv's bin directory");
        write_an_executable_script_from_a_child_process(
            &project.interpreter(),
            STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
        );
        project
    }
}

/// Undo on a running stream reaches back through a `set_pin_value` and a `set_node_config`
/// recorded before the update, each undone on the document and on the running node, and an
/// edit of an input the node no longer has is refused naming it, changing nothing.
#[tokio::test(flavor = "multi_thread")]
async fn undo_reaches_back_through_recorded_edits_and_stops_at_an_input_the_node_no_longer_has() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding_the_history_recorded_in_format_2();
    let recorded_document = project.the_graph_document_of("main");
    assert_eq!(
        recorded_document["nodes"][0]["type"],
        InputValueTestNode::node_class_import_path().as_str(),
        "the recording names the test node's type"
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let undo = json!({ "project_directory": project.path(), "author": an_agent_named("alice") });

    for each_value_undone_to in [
        json!({ "gain": 0.75, "frame_width": 1280 }),
        json!({ "gain": 0.25, "frame_width": 640 }),
    ] {
        let undone = engine
            .the_answer_or_the_refusal_of("undo", undo.clone())
            .await
            .expect("the recorded edit undoes");
        assert!(
            undone["operations"]
                .as_array()
                .is_some_and(|operations| operations
                    .iter()
                    .all(|operation| operation["operation"] == "set_node_input_value")),
            "a recorded edit undoes as input value sets: {undone}"
        );
        assert_eq!(
            the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
            each_value_undone_to
        );
        let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
        assert_eq!(mixer["input_values"], each_value_undone_to, "{mixer}");
    }

    let refused = engine
        .the_answer_or_the_refusal_of("undo", undo)
        .await
        .expect_err("`mixer` has no input `trim` to put back");
    assert!(
        refused.runtime_words.contains("has no input `trim`"),
        "{refused:?}"
    );
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
        json!({ "gain": 0.25, "frame_width": 640 }),
        "the refused undo changes nothing"
    );
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(
        mixer["input_values"],
        json!({ "gain": 0.25, "frame_width": 640 }),
        "{mixer}"
    );
}
