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
        interrupted_by_the_host: Arc::new(AtomicBool::new(false)),
    }
}

/// The cast `conversion()` makes of 0.1 into a String, its interpreter running `body`.
fn the_cast_of_a_tenth_by_an_interpreter_running(
    body: &str,
) -> std::result::Result<Value, SavedValueNotCast> {
    let project_directory = tempfile::tempdir().expect("a project directory");
    the_saved_value_cast_of_a_function_node_conversion(
        conversion(),
        "level".to_owned(),
        where_a_stub_interpreter_running(body, &project_directory),
    )
    .cast(&serde_json::json!(0.1), &PinType::String {})
}

/// Why a cast the test expects to have not run did not.
fn why_it_did_not_run(cast: std::result::Result<Value, SavedValueNotCast>) -> String {
    match cast {
        Err(SavedValueNotCast::DidNotRun(why)) => why,
        other => panic!("expected the cast not to run, got {other:?}"),
    }
}

#[test]
fn the_cast_names_the_conversion_and_a_request_file_holding_its_input_and_the_saved_value() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let where_it_runs = where_a_stub_interpreter_running("exit 0", &project_directory);
    let request_file =
        saved_value_cast_request_file(&conversion(), "level", &serde_json::json!(0.1))
            .expect("the request is written");

    let command = saved_value_cast_command(
        &conversion(),
        request_file.path(),
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
        std::path::Path::new(&arguments[at + 2]),
        request_file.path(),
        "{arguments:?}"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(request_file.path()).expect("the request file reads")
        )
        .expect("the request is JSON"),
        serde_json::json!({ "input": "level", "value": 0.1 }),
        "the keys the bootstrap reads"
    );
}

/// A String saved value longer than Linux lets one command-line argument be, 128 KiB, still
/// reaches the body whole: the stub interpreter answers with the length of the request file
/// it was handed, and the file is gone once the cast answers.
///
/// Mental-revert: passing the request as one argument fails the interpreter's start with
/// `E2BIG`, and the cast is refused as not run.
#[test]
#[serial]
fn a_saved_value_too_long_for_one_argument_reaches_the_body_whole() {
    const LONGER_THAN_ONE_ARGUMENT: usize = 200 * 1024;
    let project_directory = tempfile::tempdir().expect("a project directory");
    let request_file_named = project_directory.path().join("request-file-named");
    let where_it_runs = where_a_stub_interpreter_running(
        &format!(
            r#"for argument; do request="$argument"; done
printf '%s' "$request" > '{}'
printf '{{"cast": %s}}
' "$(wc -c < "$request")""#,
            request_file_named.display()
        ),
        &project_directory,
    );
    let saved_value = serde_json::json!("x".repeat(LONGER_THAN_ONE_ARGUMENT));

    let cast = the_saved_value_cast_of_a_function_node_conversion(
        conversion(),
        "level".to_owned(),
        where_it_runs,
    )
    .cast(&saved_value, &PinType::String {});

    let request_length = serde_json::to_string(&serde_json::json!({
        "input": "level",
        "value": saved_value,
    }))
    .expect("the request serializes")
    .len();
    assert_eq!(cast, Ok(serde_json::json!(request_length)));
    let request_file = std::fs::read_to_string(&request_file_named).expect("the stub named it");
    assert!(
        !std::path::Path::new(&request_file).exists(),
        "the request file `{request_file}` outlived its cast"
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

/// Mental-revert: reading either refusal as the other swaps the reason a retype gives.
#[test]
#[serial]
fn why_the_body_gave_no_cast_refuses_the_value_and_why_it_did_not_run_says_so() {
    assert_eq!(
        the_cast_of_a_tenth_by_an_interpreter_running(
            r#"printf '%s\n' '{"not_cast_because": "its body raised ValueError"}'"#
        ),
        Err(SavedValueNotCast::RefusedTheValue(
            "its body raised ValueError".to_owned()
        ))
    );
    assert_eq!(
        the_cast_of_a_tenth_by_an_interpreter_running(
            r#"printf '%s\n' '{"not_run_because": "it takes the stream time"}'"#
        ),
        Err(SavedValueNotCast::DidNotRun(
            "it takes the stream time".to_owned()
        ))
    );
}

#[test]
#[serial]
fn an_interpreter_that_prints_no_document_refuses_the_cast_saying_it_did_not_run() {
    let refusal = why_it_did_not_run(the_cast_of_a_tenth_by_an_interpreter_running(
        "echo broken >&2; exit 3",
    ));
    assert!(
        refusal.starts_with("its body did not run out of stream:")
            && refusal.contains("printed no cast document")
            && refusal.contains("broken"),
        "{refusal}"
    );
}

/// Mental-revert: passing a host interrupt that never fires leaves the cast running out its
/// bound.
#[test]
#[serial]
fn a_cast_the_host_interrupted_is_refused_without_running_out_its_bound() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let where_it_runs = where_a_stub_interpreter_running("sleep 30", &project_directory);
    let interrupted_by_the_host = Arc::clone(&where_it_runs.interrupted_by_the_host);
    let interrupter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        interrupted_by_the_host.store(true, Ordering::SeqCst);
    });
    let started = std::time::Instant::now();

    let refusal = why_it_did_not_run(
        the_saved_value_cast_of_a_function_node_conversion(
            conversion(),
            "level".to_owned(),
            where_it_runs,
        )
        .cast(&serde_json::json!(0.1), &PinType::String {}),
    );
    interrupter.join().expect("the interrupter ends");

    assert!(
        started.elapsed() < SAVED_VALUE_CAST_BOUND,
        "the cast held its host's interrupt for its whole bound"
    );
    assert!(
        refusal.contains("the runtime's host interrupted the cast"),
        "{refusal}"
    );
}
