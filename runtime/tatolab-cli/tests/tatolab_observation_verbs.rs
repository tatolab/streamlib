// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab graph` and `tatolab call` run as a user runs them, against a stub local API at an isolated machine's fixed
//! socket: its flags, what it prints on stdout and on stderr, and how it exits. The tool arguments
//! are the in-crate tests'.

mod common;

use common::tatolab_binary_run::{run_tatolab_reading_no_runtime_directory, standard_error_text};

/// Control is reachable only through the runtime's local API socket, so a verb dials no address.
/// Each flag carries a value: a value-taking flag given none fails the same way, so a bare flag
/// would pass whether or not the verb still took it.
#[test]
fn no_verb_takes_a_network_address_for_the_control_plane() {
    for verb_arguments in [&["graph"][..], &["graph", "--stream", "pattern"][..]] {
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
        RecordedToolCall, StubLocalApiScript, StubToolAnswer, StubToolCallTransport,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    #[test]
    fn graph_prints_every_streams_graph_the_runtime_reports() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_result(
                r#"{"machine":"desk","streams":[]}"#,
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
            "{\"machine\":\"desk\",\"streams\":[]}\n"
        );
        assert_eq!(standard_error_text(&graphed), "");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "graph".to_owned(),
                tool_arguments: json!({}),
                tool_call_transport: StubToolCallTransport::StreamableHttpPost,
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
                &serde_json::to_string_pretty(&serde_json::json!({
                    "refusal_code": "refused",
                    "action": "graph",
                    "runtime_words": "no stream named `nope` is loaded",
                }))
                .unwrap(),
            )),
        );

        let refused = isolated_machine_directories.run_tatolab(&["graph", "--stream", "nope"]);

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: graph failed: no stream named `nope` is loaded\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    #[test]
    fn call_prints_an_operations_answer_and_exits_zero() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_result(
                r#"{"files":[]}"#,
            )),
        );

        let called = isolated_machine_directories.run_tatolab(&[
            "call",
            "list_project_files",
            "--params",
            r#"{"project_directory":"/srv/rig"}"#,
        ]);

        assert!(called.status.success(), "{}", standard_error_text(&called));
        assert_eq!(standard_output_text(&called), "{\"files\":[]}\n");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "list_project_files".to_owned(),
                tool_arguments: json!({"project_directory": "/srv/rig"}),
                tool_call_transport: StubToolCallTransport::StreamableHttpPost,
            }]
        );
    }

    #[test]
    fn call_prints_the_runtimes_refusal_as_json_and_exits_one() {
        let refusal = r#"{"refusal_code":"params_do_not_match_schema","action":"graph","refused_params_pointer":"/stream","runtime_words":"invalid type"}"#;
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_failure(
                refusal,
            )),
        );

        let refused = isolated_machine_directories.run_tatolab(&[
            "call",
            "graph",
            "--params",
            r#"{"stream":7}"#,
        ]);

        assert_eq!(
            refused.status.code(),
            Some(1),
            "{}",
            standard_error_text(&refused)
        );
        assert_eq!(standard_output_text(&refused), format!("{refusal}\n"));
        assert_eq!(standard_error_text(&refused), "");
    }

    #[test]
    fn call_refuses_params_that_are_not_one_json_object_before_reaching_the_runtime() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::default());

        let refused =
            isolated_machine_directories.run_tatolab(&["call", "graph", "--params", "[]"]);

        assert_eq!(refused.status.code(), Some(2));
        assert!(
            standard_error_text(&refused).contains("the params are one JSON object"),
            "{}",
            standard_error_text(&refused)
        );
        assert!(stub_local_api_server.recorded_tool_calls().is_empty());
    }
}
