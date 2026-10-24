// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab --version` run as a user runs it, against a stub local API at an isolated machine's
//! fixed socket and against none: the CLI's version, then the runtime's line.

mod common;

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use std::path::PathBuf;

    use serde_json::json;
    use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;

    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::stub_local_api_server::{
        RecordedToolCall, StubLocalApiScript, StubToolAnswer, StubToolCallTransport,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    const CLI_VERSION_LINE: &str = concat!("tatolab ", env!("CARGO_PKG_VERSION"), "\n");

    fn a_runtime_describing_itself_as(runtime_log_directory: Option<&str>) -> StubLocalApiScript {
        let runtime_description = DescribeRuntimeToolResult {
            version: "0.55.1".to_owned(),
            build_id: "0.55.1+0123abc.9f8e7d".to_owned(),
            up_for_ms: ((3 * 24 + 2) * 60 + 7) * 60 * 1000,
            runtime_log_directory: runtime_log_directory.map(PathBuf::from),
        };
        StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_result(
            &serde_json::to_string(&runtime_description).unwrap(),
        ))
    }

    #[test]
    fn version_prints_the_clis_version_then_the_runtime_it_describes() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            a_runtime_describing_itself_as(Some("/home/u/.local/state/tatolab/logs")),
        );

        let versioned = isolated_machine_directories.run_tatolab(&["--version"]);

        assert_eq!(
            versioned.status.code(),
            Some(0),
            "{}",
            standard_error_text(&versioned)
        );
        assert_eq!(
            standard_output_text(&versioned),
            format!(
                "{CLI_VERSION_LINE}runtime 0.55.1 (0.55.1+0123abc.9f8e7d), up 3d 2h 7m, log \
                 /home/u/.local/state/tatolab/logs\n"
            )
        );
        assert_eq!(standard_error_text(&versioned), "");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "describe_runtime".to_owned(),
                tool_arguments: json!({}),
                tool_call_transport: StubToolCallTransport::StreamableHttpPost,
            }]
        );
    }

    #[test]
    fn version_of_a_runtime_keeping_no_own_log_says_so() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(a_runtime_describing_itself_as(None));

        let versioned = isolated_machine_directories.run_tatolab(&["-V"]);

        assert_eq!(versioned.status.code(), Some(0));
        assert_eq!(
            standard_output_text(&versioned),
            format!(
                "{CLI_VERSION_LINE}runtime 0.55.1 (0.55.1+0123abc.9f8e7d), up 3d 2h 7m, keeps no \
                 own log\n"
            )
        );
    }

    #[test]
    fn version_with_no_runtime_says_it_is_not_running_and_exits_0() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();

        let versioned = isolated_machine_directories.run_tatolab(&["--version"]);

        assert_eq!(
            versioned.status.code(),
            Some(0),
            "{}",
            standard_error_text(&versioned)
        );
        assert_eq!(
            standard_output_text(&versioned),
            format!("{CLI_VERSION_LINE}runtime: not running\n")
        );
        assert_eq!(standard_error_text(&versioned), "");
        assert!(
            !isolated_machine_directories
                .local_api_socket_path()
                .exists(),
            "--version starts no runtime"
        );
    }

    #[test]
    fn version_of_a_runtime_that_cannot_describe_itself_names_why_and_exits_0() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_failure(
                "no such tool",
            )),
        );

        let versioned = isolated_machine_directories.run_tatolab(&["--version"]);

        assert_eq!(versioned.status.code(), Some(0));
        assert_eq!(
            standard_output_text(&versioned),
            format!("{CLI_VERSION_LINE}runtime: describe_runtime failed: no such tool\n")
        );
    }
}
