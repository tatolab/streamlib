// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::PinType;
use serial_test::serial;

const CONVERSION: &str = "nodes.format:LevelAsText";

fn conversion() -> NodeClassImportPath {
    NodeClassImportPath::new(CONVERSION).expect("the test path names a class")
}

/// Where a conversion runs out of stream, the interpreter a shell script that runs `body`,
/// which `project_directory` holds.
fn where_a_stub_interpreter_running(
    body: &str,
    project_directory: &tempfile::TempDir,
) -> WhereAConversionRunsOutOfStream {
    let interpreter = project_directory.path().join("stub-python");
    crate::core::test_support::write_an_executable_script_from_a_child_process(
        &interpreter,
        &format!("#!/bin/sh\n{body}\n"),
    );
    WhereAConversionRunsOutOfStream {
        stream_environment: StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        },
        lend_directory: Arc::new(PathBuf::from("/opt/tatolab/lib/tatolab/lend")),
        the_stream_it_belongs_to: LoadedStreamAHelperProcessBelongsTo {
            stream_tag: crate::core::runtime::LoadedStreamTag::next_in_this_process()
                .expect("a fresh stream tag"),
            shutdown_escalation: crate::core::runtime::ShutdownEscalationOfOneStream::default(),
        },
    }
}

/// The cast `conversion()` makes of 0.1 into a String, its interpreter running `body`.
fn the_cast_of_a_tenth_by_an_interpreter_running(body: &str) -> std::result::Result<Value, String> {
    let project_directory = tempfile::tempdir().expect("a project directory");
    the_saved_value_cast_of_a_function_node_conversion(
        conversion(),
        "level".to_owned(),
        where_a_stub_interpreter_running(body, &project_directory),
    )
    .cast(&serde_json::json!(0.1), &PinType::String {})
}

#[test]
fn the_cast_names_the_conversion_its_input_and_the_saved_value_to_the_bootstrap() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let where_it_runs = where_a_stub_interpreter_running("exit 0", &project_directory);

    let command = saved_value_cast_command(
        &conversion(),
        "level",
        &serde_json::json!(0.1),
        &where_it_runs.stream_environment,
        &where_it_runs.lend_directory,
    )
    .expect("the command builds");

    let arguments: Vec<String> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let at = arguments
        .iter()
        .position(|argument| argument == CAST_A_SAVED_VALUE_ARGUMENT)
        .unwrap_or_else(|| panic!("the cast argument is passed: {arguments:?}"));
    assert_eq!(arguments[at + 1], CONVERSION, "{arguments:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&arguments[at + 2]).expect("the request is JSON"),
        serde_json::json!({ "input": "level", "value": 0.1 })
    );
}

/// Mental-revert: reading the document's `cast` as a refusal, or dropping it, refuses the cast.
#[test]
#[serial]
fn the_value_the_body_gives_is_the_cast() {
    assert_eq!(
        the_cast_of_a_tenth_by_an_interpreter_running(r#"printf '%s\n' '{"cast": "0.1 of full"}'"#),
        Ok(serde_json::json!("0.1 of full"))
    );
}

#[test]
#[serial]
fn why_the_body_gave_no_cast_is_the_refusal() {
    assert_eq!(
        the_cast_of_a_tenth_by_an_interpreter_running(
            r#"printf '%s\n' '{"not_cast_because": "it takes the stream time"}'"#
        ),
        Err("it takes the stream time".to_owned())
    );
}

#[test]
#[serial]
fn an_interpreter_that_prints_no_document_refuses_the_cast_saying_it_did_not_run() {
    let refusal = the_cast_of_a_tenth_by_an_interpreter_running("echo broken >&2; exit 3")
        .expect_err("no document is no cast");
    assert!(
        refusal.starts_with("its body did not run out of stream:")
            && refusal.contains("printed no cast document")
            && refusal.contains("broken"),
        "{refusal}"
    );
}
