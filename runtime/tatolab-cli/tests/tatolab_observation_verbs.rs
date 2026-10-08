// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab nodes`, `graph` and `tap` run as a user runs them, against a stub local API. The
//! registry is isolated through `XDG_RUNTIME_DIR`, which only Linux honours, so the tests that
//! read one are Linux-only; the in-crate tests drive the same logic on every floor.

mod common;

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

fn verbs_listed_in_help() -> Vec<String> {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&["--help"]));
    help_text
        .lines()
        .skip_while(|help_line| !help_line.starts_with("Commands:"))
        .skip(1)
        .take_while(|help_line| !help_line.trim().is_empty())
        .map(|help_line| help_line.split_whitespace().next().unwrap().to_owned())
        .collect()
}

#[test]
fn every_observation_verb_is_a_subcommand() {
    let listed_verbs = verbs_listed_in_help();
    for observation_verb in ["nodes", "graph", "tap"] {
        assert!(
            listed_verbs
                .iter()
                .any(|listed_verb| listed_verb == observation_verb),
            "`tatolab {observation_verb}` must be a served verb: {listed_verbs:?}"
        );
    }
}

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

#[test]
fn a_retired_nodes_flag_is_a_usage_error() {
    // Each spelled from its parts so the retired flag's own text does not survive here.
    for retired_flag in [
        ["mesh", "name"].as_slice(),
        &["mesh", "peer"],
        &["no", "mesh", "multicast", "discovery"],
    ]
    .map(|flag_words| format!("--{}", flag_words.join("-")))
    {
        let refused = run_tatolab_reading_no_runtime_directory(&["nodes", &retired_flag]);

        assert_eq!(refused.status.code(), Some(2), "{retired_flag}");
        assert!(
            standard_error_text(&refused).contains(&retired_flag),
            "{retired_flag}"
        );
    }
}

#[cfg(target_os = "linux")]
mod against_an_isolated_registry {
    use serde_json::json;

    use super::common::isolated_node_registry::{
        IsolatedNodeRegistry, a_registry_entry, a_registry_entry_named,
    };
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
    fn nodes_renders_a_live_runtime_as_a_table() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rlisted",
            "rig-desk-a1b2",
            &stub_local_api_server.local_api_socket_path,
        ));

        let listed =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["nodes"]);

        assert!(listed.status.success(), "{}", standard_error_text(&listed));
        let printed = standard_output_text(&listed);
        let header = printed.lines().next().unwrap();
        assert!(
            header.starts_with("RUNTIME_NAME"),
            "the runtime's name is the first column a reader sees: {header:?}"
        );
        assert!(header.contains("RUNTIME_ID"), "{header}");
        assert!(header.contains("LOCAL_API_SOCKET"), "{header}");
        assert!(!header.contains(&["CONTROL", "URL"].join("_")), "{header}");
        assert!(printed.contains("rig-desk-a1b2"), "{printed}");
        assert!(printed.contains("Rlisted"), "{printed}");
        assert!(
            printed.contains(
                &stub_local_api_server
                    .local_api_socket_path
                    .display()
                    .to_string()
            ),
            "{printed}"
        );
        assert!(printed.contains("yes"), "{printed}");
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
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result(r#"{"nodes":[]}"#),
        );
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

        let graphed =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["graph"]);

        assert!(
            graphed.status.success(),
            "{}",
            standard_error_text(&graphed)
        );
        assert_eq!(standard_output_text(&graphed), "{\"nodes\":[]}\n");
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
    fn a_node_flag_naming_nothing_is_refused_listing_what_is_live() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rnamed",
            "rig-desk-a1b2",
            &stub_local_api_server.local_api_socket_path,
        ));

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "rig/pattern/video", "--node", "rig-nowhere-0000"],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            format!(
                "error: no live runtime named `rig-nowhere-0000`, and none with that runtime_id. \
                 Live runtimes: rig-desk-a1b2 (Rnamed) -> {}\n",
                stub_local_api_server.local_api_socket_path.display()
            )
        );
    }

    #[test]
    fn a_verb_with_no_runtime_running_names_the_command_that_starts_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let refused =
            run_tatolab_with_xdg_runtime_dir(isolated_node_registry.xdg_runtime_dir(), &["graph"]);

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: no running runtime found on this machine.\nStart one with `tatolab run`.\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    #[test]
    fn a_tool_level_error_is_a_refusal_not_a_printed_result() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_failure("no such channel"),
        );
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

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
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

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

    /// A bag over the tool's per-bag cap comes back undecodable, so a caller that raised the cap
    /// and had the flag silently dropped would get exactly the failure it was trying to avoid.
    #[test]
    fn tap_forwards_a_named_per_bag_cap() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

        let tapped = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "cam/video", "--max-bag-bytes", "4096"],
        );

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"channel": "cam/video", "max_bag_bytes": 4096})
        );
    }

    /// Absent means absent: the tool's own default applies, and the CLI invents none of its own.
    #[test]
    fn tap_omits_a_per_bag_cap_nobody_named() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

        let tapped = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["tap", "cam/video"],
        );

        assert!(tapped.status.success(), "{}", standard_error_text(&tapped));
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"channel": "cam/video"})
        );
    }

    /// The runtime owns the bounds: a count it will refuse is sent as given, as the Python verb
    /// sent it, rather than refused here in other words.
    #[test]
    fn tap_forwards_a_negative_count_for_the_runtime_to_judge() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

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
