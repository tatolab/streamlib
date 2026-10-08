// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab nodes`, `graph` and `tap` run as a user runs them, against a stub local API: their
//! flags, what they print on stdout and on stderr, and how they exit. Selection, rendering and
//! the tool arguments are the in-crate tests'. The registry is isolated through
//! `XDG_RUNTIME_DIR`, which only Linux honours, so the tests that read one are Linux-only.

mod common;

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

#[test]
fn the_nodes_help_names_every_column_it_prints() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "nodes", "--help",
    ]));

    for column in [
        "runtime_name",
        "runtime_id",
        "local_api_socket",
        "pid",
        "alive?",
        "hint",
    ] {
        assert!(
            help_text.contains(column),
            "`nodes --help` must document {column}:\n{help_text}"
        );
    }
}

#[test]
fn the_tap_help_names_the_channel_its_bounds_and_the_runtime_flag() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "tap", "--help",
    ]));

    for named in [
        "<CHANNEL>",
        "<runtime_name>/<node>/<port>",
        "--count <N>",
        "--max-bag-bytes <BYTES>",
        "--node <RUNTIME_NAME_OR_ID>",
        "never blocks the producer",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
}

/// Control is reachable only through a runtime's local API socket, so a verb dials no address.
/// Each flag carries a value: a value-taking flag given none fails the same way, so a bare flag
/// would pass whether or not the verb still took it.
#[test]
fn no_verb_takes_a_network_address_for_the_control_plane() {
    for verb_arguments in [&["graph"][..], &["tap", "rig/pattern/video"][..]] {
        let refused = run_tatolab_reading_no_runtime_directory(
            &[verb_arguments, &["--url", "http://127.0.0.1:9100"]].concat(),
        );

        assert_eq!(refused.status.code(), Some(2), "{verb_arguments:?}");
        let refusal = standard_error_text(&refused);
        assert!(refusal.contains("unexpected argument '--url'"), "{refusal}");
    }
}

/// `nodes` takes no flag but `--help`, so any flag it once took is a usage error.
#[test]
fn a_retired_nodes_flag_is_a_usage_error() {
    let nodes_help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "nodes", "--help",
    ]));
    let nodes_long_flags: Vec<&str> = nodes_help_text
        .lines()
        .skip_while(|help_line| !help_line.starts_with("Options:"))
        .skip(1)
        .take_while(|help_line| !help_line.trim().is_empty())
        .filter(|help_line| help_line.trim_start().starts_with('-'))
        .flat_map(|option_line| option_line.split_whitespace())
        .filter(|option_word| option_word.starts_with("--"))
        .collect();
    assert_eq!(nodes_long_flags, ["--help"], "{nodes_help_text}");

    let refused = run_tatolab_reading_no_runtime_directory(&["nodes", "--name", "rig"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("unexpected argument '--name'"),
        "{}",
        standard_error_text(&refused)
    );
}

#[cfg(target_os = "linux")]
mod against_an_isolated_registry {
    use serde_json::json;

    use super::common::isolated_node_registry::{IsolatedNodeRegistry, a_registry_entry_named};
    use super::common::stub_local_api_server::{
        RecordedToolCall, StubLocalApiServer, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{
        run_tatolab_with_xdg_runtime_dir, standard_error_text, standard_output_text,
    };

    #[test]
    fn nodes_reports_an_empty_registry_without_failing_naming_the_registry_xdg_runtime_dir_holds() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let listed =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["nodes"]);

        assert!(listed.status.success(), "{}", standard_error_text(&listed));
        assert_eq!(
            standard_output_text(&listed),
            format!(
                "No running nodes found in {}.\n",
                isolated_node_registry
                    .xdg_runtime_dir()
                    .join("streamlib")
                    .join("nodes")
                    .display()
            )
        );
    }

    #[test]
    fn nodes_prints_the_registry_table_alone() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let first_stub_local_api_server = StubLocalApiServer::serve_default();
        let second_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rfirst",
            "rig-desk-a1b2",
            &first_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rsecond",
            "rig-lab-c3d4",
            &second_stub_local_api_server.local_api_socket_path,
        ));

        let listed =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["nodes"]);

        assert!(listed.status.success(), "{}", standard_error_text(&listed));
        let printed = standard_output_text(&listed);
        let printed_lines: Vec<&str> = printed.lines().collect();
        assert_eq!(
            printed_lines.len(),
            3,
            "`nodes` prints a header and one row per registry entry, nothing else: \
             {printed_lines:?}"
        );
        assert!(printed_lines[0].starts_with("RUNTIME_NAME"), "{printed}");
        let mut listed_runtime_names: Vec<&str> = printed_lines[1..]
            .iter()
            .map(|row| row.split_whitespace().next().unwrap())
            .collect();
        listed_runtime_names.sort_unstable();
        assert_eq!(listed_runtime_names, ["rig-desk-a1b2", "rig-lab-c3d4"]);
        assert_eq!(standard_error_text(&listed), "");
    }

    #[test]
    fn graph_prints_the_tool_result() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result(r#"{"nodes":[]}"#),
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let graphed =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["graph"]);

        assert!(
            graphed.status.success(),
            "{}",
            standard_error_text(&graphed)
        );
        assert_eq!(standard_output_text(&graphed), "{\"nodes\":[]}\n");
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
    fn a_verb_targets_a_runtime_by_its_runtime_name() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let named_stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result("the named runtime's graph"),
        );
        let other_stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result("the other runtime's graph"),
        );
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rnamed",
            "rig-desk-a1b2",
            &named_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rother",
            "rig-lab-c3d4",
            &other_stub_local_api_server.local_api_socket_path,
        ));

        let graphed = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["graph", "--node", "rig-desk-a1b2"],
        );

        assert!(
            graphed.status.success(),
            "{}",
            standard_error_text(&graphed)
        );
        assert_eq!(
            standard_output_text(&graphed),
            "the named runtime's graph\n"
        );
        assert_eq!(other_stub_local_api_server.recorded_tool_calls(), []);
    }

    #[test]
    fn a_verb_given_a_name_two_live_runtimes_hold_is_refused_naming_both() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let first_stub_local_api_server = StubLocalApiServer::serve_default();
        let second_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rfirst",
            "rig-desk-a1b2",
            &first_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rsecond",
            "rig-desk-a1b2",
            &second_stub_local_api_server.local_api_socket_path,
        ));

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["graph", "--node", "rig-desk-a1b2"],
        );

        assert_eq!(refused.status.code(), Some(1));
        let refusal = standard_error_text(&refused);
        assert!(
            refusal.starts_with("error: 2 live runtimes answer to `rig-desk-a1b2`"),
            "{refusal}"
        );
        for named_row in [
            "Rfirst".to_owned(),
            first_stub_local_api_server
                .local_api_socket_path
                .display()
                .to_string(),
            "Rsecond".to_owned(),
            second_stub_local_api_server
                .local_api_socket_path
                .display()
                .to_string(),
        ] {
            assert!(refusal.contains(&named_row), "{named_row}: {refusal}");
        }
        assert_eq!(
            standard_output_text(&refused),
            "",
            "a refused name prints neither runtime's graph"
        );
        assert_eq!(first_stub_local_api_server.recorded_tool_calls(), []);
        assert_eq!(second_stub_local_api_server.recorded_tool_calls(), []);
    }

    #[test]
    fn a_tool_level_error_is_a_refusal_not_a_printed_result() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_failure("no such channel"),
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "nope"],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: tap failed: no such channel\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    #[test]
    fn tap_sends_the_channel_and_count() {
        let stub_local_api_server = StubLocalApiServer::serve_default();
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let tapped = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "cam/video", "--count", "3"],
        );

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(standard_output_text(&tapped), "{}\n");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "tap".to_owned(),
                tool_arguments: json!({"channel": "cam/video", "count": 3}),
            }]
        );
    }

    /// The runtime owns the bounds, so a count it will refuse is sent as given rather than
    /// refused here in other words.
    #[test]
    fn tap_forwards_a_negative_count_for_the_runtime_to_judge() {
        let stub_local_api_server = StubLocalApiServer::serve_default();
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let tapped = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "cam/video", "--count", "-1"],
        );

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"channel": "cam/video", "count": -1})
        );
    }
}
