// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab graph` and `tap` run as a user runs them, against a stub local API at an isolated
//! machine's fixed socket: their flags, what they print on stdout and on stderr, and how they
//! exit. The tool arguments are the in-crate tests'.

mod common;

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

#[test]
fn the_tap_help_names_the_channel_its_stream_and_its_bounds() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "tap", "--help",
    ]));

    for named in [
        "<CHANNEL>",
        "<runtime_name>/<node>/<port>",
        "--stream <STREAM>",
        "--count <N>",
        "--max-bag-bytes <BYTES>",
        "never blocks the producer",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
    assert!(
        !help_text.contains('`'),
        "help reads plainly, with no markdown:\n{help_text}"
    );
}

/// A tap names the stream its channel belongs to: the runtime holds many.
#[test]
fn tap_without_a_stream_is_a_usage_error() {
    let refused = run_tatolab_reading_no_runtime_directory(&["tap", "rig/pattern/video"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("--stream <STREAM>"),
        "{}",
        standard_error_text(&refused)
    );
}

/// Control is reachable only through the runtime's local API socket, so a verb dials no address.
/// Each flag carries a value: a value-taking flag given none fails the same way, so a bare flag
/// would pass whether or not the verb still took it.
#[test]
fn no_verb_takes_a_network_address_for_the_control_plane() {
    for verb_arguments in [
        &["graph"][..],
        &["tap", "rig/pattern/video", "--stream", "pattern"][..],
    ] {
        let refused = run_tatolab_reading_no_runtime_directory(
            &[verb_arguments, &["--url", "http://127.0.0.1:9100"]].concat(),
        );

        assert_eq!(refused.status.code(), Some(2), "{verb_arguments:?}");
        let refusal = standard_error_text(&refused);
        assert!(refusal.contains("unexpected argument '--url'"), "{refusal}");
    }
}

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use serde_json::json;

    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::stub_local_api_server::{
        RecordedToolCall, StubLocalApiScript, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    #[test]
    fn graph_prints_every_streams_graph_the_runtime_reports() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_result(
                r#"{"runtime_name":"desk","streams":[]}"#,
            )),
        );

        let graphed = isolated_machine_directories.run_tatolab(&["graph"]);

        assert!(
            graphed.status.success(),
            "{}",
            standard_error_text(&graphed)
        );
        assert_eq!(
            standard_output_text(&graphed),
            "{\"runtime_name\":\"desk\",\"streams\":[]}\n"
        );
        assert_eq!(standard_error_text(&graphed), "");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "graph".to_owned(),
                tool_arguments: json!({}),
            }]
        );
    }

    #[test]
    fn graph_of_one_stream_names_it() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::default());

        let graphed = isolated_machine_directories.run_tatolab(&["graph", "--stream", "camera"]);

        assert!(
            graphed.status.success(),
            "{}",
            standard_error_text(&graphed)
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("graph"),
            [json!({"stream": "camera"})]
        );
    }

    #[test]
    fn a_tool_level_error_is_a_refusal_not_a_printed_result() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_failure(
                "no such channel",
            )),
        );

        let refused = isolated_machine_directories.run_tatolab(&["tap", "nope", "--stream", "s"]);

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: tap failed: no such channel\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    #[test]
    fn tap_sends_the_stream_the_channel_and_the_count() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::default());

        let tapped = isolated_machine_directories.run_tatolab(&[
            "tap",
            "cam/video",
            "--stream",
            "camera",
            "--count",
            "3",
        ]);

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(standard_output_text(&tapped), "{}\n");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "tap".to_owned(),
                tool_arguments: json!({"stream": "camera", "channel": "cam/video", "count": 3}),
            }]
        );
    }

    /// The runtime owns the bounds, so a count it will refuse is sent as given rather than
    /// refused here in other words.
    #[test]
    fn tap_forwards_a_negative_count_for_the_runtime_to_judge() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::default());

        let tapped = isolated_machine_directories.run_tatolab(&[
            "tap",
            "cam/video",
            "--stream",
            "camera",
            "--count",
            "-1",
        ]);

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("tap"),
            [json!({"stream": "camera", "channel": "cam/video", "count": -1})]
        );
    }
}
