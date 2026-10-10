// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab run -d`, `stop`, `start`, `rm`, `streams` and `expose` run as a user runs them,
//! against a stub local API at an isolated machine's fixed socket: the tool each calls, the
//! arguments it sends, the one line or table it prints, and how a refusal reads.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::stub_local_api_server::{
    RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubToolAnswer,
    StubToolCallTransport, run_stream_tool_result_text, stop_stream_tool_result_text,
};
use common::tatolab_binary_run::{standard_error_text, standard_output_text};

/// A machine whose runtime answers `tool_name` with `tool_answer`.
fn a_runtime_answering(
    tool_name: &str,
    tool_answer: StubToolAnswer,
) -> (IsolatedMachineDirectories, StubLocalApiServer) {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([(tool_name.to_owned(), vec![tool_answer])]),
            ..StubLocalApiScript::default()
        });
    (isolated_machine_directories, stub_local_api_server)
}

/// Run `tatolab_arguments` to a successful exit, answering what it printed.
fn printed_by_a_successful(
    isolated_machine_directories: &IsolatedMachineDirectories,
    tatolab_arguments: &[&str],
) -> String {
    let finished = isolated_machine_directories.run_tatolab(tatolab_arguments);
    assert_eq!(
        finished.status.code(),
        Some(0),
        "{tatolab_arguments:?}: {}",
        standard_error_text(&finished)
    );
    assert_eq!(standard_error_text(&finished), "", "{tatolab_arguments:?}");
    standard_output_text(&finished)
}

#[test]
fn run_detached_hands_the_stream_to_the_runtime_to_keep_and_prints_one_line() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "cam",
            true,
            &canonical_project_directory,
            3,
            &[],
        )),
    );

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "run",
            "-d",
            "stream.py:main",
            "--dir",
            project_directory.path().to_str().unwrap(),
            "--name",
            "cam",
        ],
    );

    assert_eq!(
        printed,
        format!(
            "cam kept (project {})\n",
            canonical_project_directory.display()
        )
    );
    assert_eq!(
        stub_local_api_server.recorded_tool_calls(),
        [RecordedToolCall {
            tool_name: "run_stream".to_owned(),
            tool_arguments: json!({
                "project_directory": canonical_project_directory,
                "stream_function": "stream.py:main",
                "name": "cam",
                "keep": true,
            }),
            tool_call_transport: StubToolCallTransport::StreamableHttpPost,
        }]
    );
    assert_eq!(
        stub_local_api_server.recorded_mcp_stdio_request_heads(),
        [],
        "a kept load is one call over POST /mcp, never a connection of its own"
    );
}

/// The compile's standard error reaches the user's terminal: each line on `tatolab`'s stderr,
/// and the kept line alone on its stdout.
#[test]
fn run_detached_prints_each_line_the_compile_wrote_on_its_standard_error() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let compile_warnings = [
        "tatolab: the cross-floor check found 1 thing binding this app to one floor (Linux or macOS). The app starts anyway.",
        "  processors/effect.py:4: imports `cupy`",
    ];
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "cam",
            true,
            &canonical_project_directory,
            3,
            &compile_warnings,
        )),
    );

    let finished = isolated_machine_directories.run_tatolab(&[
        "run",
        "-d",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(
        standard_error_text(&finished),
        format!("{}\n{}\n", compile_warnings[0], compile_warnings[1])
    );
    assert_eq!(
        standard_output_text(&finished),
        format!(
            "cam kept (project {})\n",
            canonical_project_directory.display()
        )
    );
}

#[test]
fn run_detached_sends_a_file_as_the_stream_function() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "other",
            true,
            &canonical_project_directory,
            1,
            &[],
        )),
    );

    printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "run",
            "--detach",
            "-f",
            "other.py",
            "--dir",
            project_directory.path().to_str().unwrap(),
        ],
    );

    assert_eq!(
        stub_local_api_server.recorded_arguments_of("run_stream"),
        [json!({
            "project_directory": canonical_project_directory,
            "stream_function": "other.py",
            "keep": true,
        })]
    );
}

#[test]
fn a_refused_detached_load_prints_the_runtimes_refusal_and_exits_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_failure(
            "a stream named `cam` is already loaded from /srv/other; load it under another name \
             with --name",
        ),
    );

    let refused = isolated_machine_directories.run_tatolab(&[
        "run",
        "-d",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: run_stream failed: a stream named `cam` is already loaded from /srv/other; load \
         it under another name with --name\n"
    );
    assert_eq!(standard_output_text(&refused), "");
}

#[test]
fn stop_unloads_the_stream_and_says_a_kept_one_stays_stopped() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("cam", true)),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["stop", "cam"]);

    assert_eq!(
        printed,
        "cam stopped; it stays stopped across restarts until `tatolab start cam`\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        [json!({"stream": "cam"})]
    );
}

#[test]
fn a_kept_stop_the_runtime_could_not_record_warns_naming_why_and_exits_zero() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "stop_stream",
        StubToolAnswer::tool_result(
            &json!({
                "stream": "cam",
                "stopped": true,
                "kept": true,
                "not_recorded_because": "the record /state/streams/cam.json cannot be read",
            })
            .to_string(),
        ),
    );

    let finished = isolated_machine_directories.run_tatolab(&["stop", "cam"]);

    assert_eq!(finished.status.code(), Some(0), "the stop took effect");
    assert_eq!(standard_output_text(&finished), "cam stopped\n");
    assert_eq!(
        standard_error_text(&finished),
        "warning: cam was not recorded stopped, so a restart of the runtime loads it again: the \
         record /state/streams/cam.json cannot be read\n"
    );
}

#[test]
fn start_loads_a_stopped_stream_and_names_its_node_count() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "start_stream",
        StubToolAnswer::tool_result(r#"{"stream":"cam","node_count":4}"#),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["start", "cam"]);

    assert_eq!(printed, "cam started (4 nodes)\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("start_stream"),
        [json!({"stream": "cam"})]
    );
}

#[test]
fn rm_unloads_and_forgets_the_stream() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "remove_stream",
        StubToolAnswer::tool_result(r#"{"stream":"cam","unloaded":true,"forgotten":true}"#),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["rm", "cam"]);

    assert_eq!(printed, "cam removed (unloaded and forgotten)\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("remove_stream"),
        [json!({"stream": "cam"})]
    );
}

#[test]
fn streams_prints_the_table_of_what_the_runtime_holds() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "list_streams",
        StubToolAnswer::tool_result(
            &json!({"streams": [
                {"name": "cam", "state": "kept", "project_directory": "/srv/cam", "node_count": 3},
                {"name": "mic", "state": "stopped", "project_directory": "/srv/mic", "node_count": null},
            ]})
            .to_string(),
        ),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["streams"]);

    assert_eq!(
        printed,
        "NAME  STATE    NODES  PROJECT\n\
         cam   kept     3      /srv/cam\n\
         mic   stopped  -      /srv/mic\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("list_streams"),
        [json!({})]
    );
}

#[test]
fn streams_says_so_when_the_runtime_holds_none() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "list_streams",
        StubToolAnswer::tool_result(r#"{"streams":[]}"#),
    );

    assert_eq!(
        printed_by_a_successful(&isolated_machine_directories, &["streams"]),
        "No streams in this runtime.\n"
    );
}

#[test]
fn expose_sends_private_public_or_internal_by_its_flag() {
    for (level_flags, wire_level) in [
        (&[][..], "private"),
        (&["--public"][..], "public"),
        (&["--remove"][..], "internal"),
    ] {
        let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
            "expose_port",
            StubToolAnswer::tool_result(
                &json!({
                    "stream": "cam",
                    "node": "effect",
                    "port": "video",
                    "level": wire_level,
                    "recorded": true,
                })
                .to_string(),
            ),
        );

        let printed = printed_by_a_successful(
            &isolated_machine_directories,
            &[&["expose", "cam", "effect", "video"], level_flags].concat(),
        );

        assert_eq!(
            printed,
            format!("cam/effect/video is {wire_level} (recorded; it holds across restarts)\n")
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("expose_port"),
            [json!({"stream": "cam", "node": "effect", "port": "video", "level": wire_level})],
            "{level_flags:?}"
        );
    }
}

#[test]
fn a_kept_exposure_the_runtime_could_not_record_warns_naming_why_and_exits_zero() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "expose_port",
        StubToolAnswer::tool_result(
            &json!({
                "stream": "cam",
                "node": "effect",
                "port": "video",
                "level": "public",
                "recorded": false,
                "not_recorded_because": "the record /state/streams/cam.json cannot be written",
            })
            .to_string(),
        ),
    );

    let finished =
        isolated_machine_directories.run_tatolab(&["expose", "cam", "effect", "video", "--public"]);

    assert_eq!(finished.status.code(), Some(0), "the level changed live");
    assert_eq!(
        standard_output_text(&finished),
        "cam/effect/video is public (live only: not recorded)\n"
    );
    assert_eq!(
        standard_error_text(&finished),
        "warning: cam/effect/video changed live but was not recorded as the owner's ruling, so a \
         restart of the runtime puts back the level it had: the record \
         /state/streams/cam.json cannot be written\n"
    );
}

#[test]
fn a_kept_restriction_the_runtime_refused_for_its_record_exits_one_printing_nothing_on_stdout() {
    let refusal = "the port `effect/video` of the kept stream `cam` was not restricted to \
                   internal, and is still public: the owner's ruling could not be recorded";
    let (isolated_machine_directories, _stub_local_api_server) =
        a_runtime_answering("expose_port", StubToolAnswer::tool_failure(refusal));

    let refused =
        isolated_machine_directories.run_tatolab(&["expose", "cam", "effect", "video", "--remove"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        format!("error: expose_port failed: {refusal}\n")
    );
    assert_eq!(standard_output_text(&refused), "");
}

#[test]
fn a_stream_the_runtime_does_not_hold_is_refused_in_its_words() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "stop_stream",
        StubToolAnswer::tool_failure("no stream `nope`; this runtime holds: cam"),
    );

    let refused = isolated_machine_directories.run_tatolab(&["stop", "nope"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: stop_stream failed: no stream `nope`; this runtime holds: cam\n"
    );
}
