// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab relays`, `relay add`, `relay set` and `relay rm` run as a user runs them, against a
//! stub local API at an isolated machine's fixed socket: the tool each calls, the arguments it
//! sends, what it prints, and that a secret is read from a file or stdin and never taken as an
//! argument.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;
use std::io::Write as _;
use std::process::Stdio;

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::stub_local_api_server::{StubLocalApiScript, StubLocalApiServer, StubToolAnswer};
use common::tatolab_binary_run::{standard_error_text, standard_output_text};

/// What `list_relays` answers: the local relay's loopback listener, built in, and a provider
/// relay whose `token` is a set secret.
fn list_relays_tool_result_text() -> String {
    json!({
        "relays": [
            {
                "relay_id": "local-loopback",
                "adapter": "local",
                "built_in": true,
                "settings": {},
                "secrets": {},
                "connection_state": {"state": "joined"}
            },
            {
                "relay_id": "provider-1",
                "adapter": "provider",
                "built_in": false,
                "settings": {"region": "us"},
                "secrets": {"token": "set"},
                "connection_state": {"state": "lost", "why": "refused", "rejoining_after_ms": 2000}
            }
        ]
    })
    .to_string()
}

/// A machine whose runtime answers `list_relays` with two relays and each tool in
/// `tool_answers` as given.
fn a_runtime_holding_two_relays_answering(
    tool_answers: &[(&str, StubToolAnswer)],
) -> (IsolatedMachineDirectories, StubLocalApiServer) {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let mut tool_answers_by_name = HashMap::from([(
        "list_relays".to_owned(),
        vec![StubToolAnswer::tool_result(&list_relays_tool_result_text())],
    )]);
    for (tool_name, tool_answer) in tool_answers {
        tool_answers_by_name.insert((*tool_name).to_owned(), vec![tool_answer.clone()]);
    }
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name,
            ..StubLocalApiScript::default()
        });
    (isolated_machine_directories, stub_local_api_server)
}

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
    standard_output_text(&finished)
}

#[test]
fn relays_prints_each_relay_with_its_secrets_set_or_unset_and_why_one_is_not_joined() {
    let (isolated_machine_directories, _stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[]);
    assert_eq!(
        printed_by_a_successful(&isolated_machine_directories, &["relays"]),
        "RELAY                      ADAPTER   STATE   SETTINGS         SECRETS\n\
         local-loopback (built in)  local     joined  {}\n\
         provider-1                 provider  lost    {\"region\":\"us\"}  token set\n\
         \n\
         provider-1 is lost (refused); joining again in 2000 ms\n"
    );
}

#[test]
fn relay_add_hands_the_runtime_the_adapter_and_the_settings_file_and_prints_the_id() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[(
            "add_relay",
            StubToolAnswer::tool_result(r#"{"relay_id":"provider-2"}"#),
        )]);
    let settings_directory = tempfile::tempdir().unwrap();
    let settings_file = settings_directory.path().join("provider.json");
    std::fs::write(
        &settings_file,
        r#"{"region": "eu", "token": "from-a-file"}"#,
    )
    .unwrap();

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "relay",
            "add",
            "provider",
            "--settings-file",
            settings_file.to_str().unwrap(),
        ],
    );

    assert_eq!(printed, "provider-2 added (adapter provider)\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("add_relay"),
        [json!({"adapter": "provider", "settings": {"region": "eu", "token": "from-a-file"}})]
    );
}

#[test]
fn relay_set_replaces_each_key_value_read_as_json_when_it_parses() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[(
            "configure_relay",
            StubToolAnswer::tool_result(r#"{"relay_id":"local-loopback"}"#),
        )]);

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &["relay", "set", "local-loopback", "port=4500", "label=desk"],
    );

    assert_eq!(printed, "local-loopback configured\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("configure_relay"),
        [json!({"relay_id": "local-loopback", "settings": {"port": 4500, "label": "desk"}})]
    );
}

#[test]
fn relay_set_refuses_a_secret_given_as_an_argument_by_name_and_sends_nothing() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[]);

    let refused =
        isolated_machine_directories.run_tatolab(&["relay", "set", "provider-1", "token=s3cr3t"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: `token` is a secret of the relay `provider-1`, and a secret is never an \
         argument: `tatolab relay set provider-1 --secret token --from-file <path>` or \
         `--from-stdin`\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("configure_relay"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn relay_set_reads_a_secret_once_from_a_file_dropping_its_line_ending() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[(
            "configure_relay",
            StubToolAnswer::tool_result(r#"{"relay_id":"provider-1"}"#),
        )]);
    let secret_directory = tempfile::tempdir().unwrap();
    let secret_file = secret_directory.path().join("token");
    std::fs::write(&secret_file, "rotated-token\n").unwrap();

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "relay",
            "set",
            "provider-1",
            "--secret",
            "token",
            "--from-file",
            secret_file.to_str().unwrap(),
        ],
    );

    assert_eq!(printed, "provider-1 configured\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("configure_relay"),
        [json!({"relay_id": "provider-1", "settings": {"token": "rotated-token"}})]
    );
}

#[test]
fn relay_set_reads_a_secret_once_from_standard_input() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[(
            "configure_relay",
            StubToolAnswer::tool_result(r#"{"relay_id":"provider-1"}"#),
        )]);

    let mut tatolab = isolated_machine_directories
        .tatolab_command(&[
            "relay",
            "set",
            "provider-1",
            "region=eu",
            "--secret",
            "token",
            "--from-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tatolab
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped-token")
        .unwrap();
    let finished = tatolab.wait_with_output().unwrap();

    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("configure_relay"),
        [json!({"relay_id": "provider-1", "settings": {"region": "eu", "token": "piped-token"}})]
    );
}

#[test]
fn relay_set_with_a_secret_and_no_source_is_refused_before_any_call() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[]);

    let refused = isolated_machine_directories.run_tatolab(&[
        "relay",
        "set",
        "provider-1",
        "--secret",
        "token",
    ]);

    assert_eq!(
        refused.status.code(),
        Some(2),
        "{}",
        standard_error_text(&refused)
    );
    assert!(stub_local_api_server.recorded_tool_calls().is_empty());
}

#[test]
fn relay_rm_of_a_built_in_relay_prints_the_runtimes_refusal_and_exits_one() {
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_holding_two_relays_answering(&[(
            "remove_relay",
            StubToolAnswer::tool_failure(
                "the relay `local-loopback` is built in and is never removed",
            ),
        )]);

    let refused = isolated_machine_directories.run_tatolab(&["relay", "rm", "local-loopback"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: remove_relay failed: the relay `local-loopback` is built in and is never \
         removed\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("remove_relay"),
        [json!({"relay_id": "local-loopback"})]
    );
}
