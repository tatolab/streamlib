// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab --help` lists the served verbs, and `tatolab new` is one `create_project` call on
//! the machine's runtime: the directory it sends, the lines it prints, and how a refusal reads.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::stub_local_api_server::{
    RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubToolAnswer, StubToolCallTransport,
};
use common::tatolab_binary_run::{standard_error_text, standard_output_text};

fn run_tatolab(working_directory: &Path, tatolab_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .args(tatolab_arguments)
        .current_dir(working_directory)
        .output()
        .unwrap()
}

/// A machine whose runtime answers `create_project` with `tool_answer`.
fn a_runtime_answering_create_project(
    tool_answer: StubToolAnswer,
) -> (IsolatedMachineDirectories, StubLocalApiServer) {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([("create_project".to_owned(), vec![tool_answer])]),
            ..StubLocalApiScript::default()
        });
    (isolated_machine_directories, stub_local_api_server)
}

/// What a runtime's `create_project` answers once it queued `action` creating `project_directory`.
fn create_project_tool_result_text(action: &str, project_directory: &Path) -> String {
    json!({
        "action": action,
        "stream": "brisk-otter-main-0a1b2c",
        "project_directory": project_directory,
    })
    .to_string()
}

/// Run `tatolab_arguments` in `working_directory` to its exit.
fn run_tatolab_in(
    isolated_machine_directories: &IsolatedMachineDirectories,
    working_directory: &Path,
    tatolab_arguments: &[&str],
) -> Output {
    isolated_machine_directories
        .tatolab_command(tatolab_arguments)
        .current_dir(working_directory)
        .output()
        .unwrap()
}

#[test]
fn help_lists_exactly_the_served_verbs() {
    let scratch_directory = tempfile::tempdir().unwrap();
    let help_output = run_tatolab(scratch_directory.path(), &["--help"]);
    assert!(help_output.status.success());
    let help_text = String::from_utf8(help_output.stdout).unwrap();
    let listed_verbs: Vec<String> = help_text
        .lines()
        .skip_while(|help_line| !help_line.starts_with("Commands:"))
        .skip(1)
        .take_while(|help_line| !help_line.trim().is_empty())
        .map(|help_line| help_line.split_whitespace().next().unwrap().to_owned())
        .collect();
    assert_eq!(
        listed_verbs,
        [
            "new",
            "run",
            "dev",
            "stop",
            "start",
            "rm",
            "cancel",
            "rename",
            "streams",
            "expose",
            "relays",
            "relay",
            "graph",
            "logs",
            "mcp",
            "enable-virtual-camera"
        ],
        "help was:\n{help_text}"
    );
}

#[test]
fn new_in_a_directory_creates_the_project_there_through_the_runtime_and_says_how_to_follow_it() {
    let working_directory = tempfile::tempdir().unwrap();
    // `new` sends the directory resolved, and macOS's temporary directory is under a symlink.
    let project_directory = working_directory
        .path()
        .canonicalize()
        .unwrap()
        .join("My Probe App");
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_answering_create_project(StubToolAnswer::tool_result(
            &create_project_tool_result_text("Rabc-1", &project_directory),
        ));

    let finished = run_tatolab_in(
        &isolated_machine_directories,
        working_directory.path(),
        &["new", "My Probe App"],
    );

    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(
        standard_output_text(&finished),
        format!(
            "Rabc-1 queued: create of {0}\n  follow it: tatolab logs -f --dir {0}\n",
            project_directory.display()
        )
    );
    assert_eq!(
        stub_local_api_server.recorded_tool_calls(),
        [RecordedToolCall {
            tool_name: "create_project".to_owned(),
            tool_arguments: json!({ "project_directory": project_directory }),
            tool_call_transport: StubToolCallTransport::StreamableHttpPost,
        }]
    );
    assert!(
        !project_directory.exists(),
        "the runtime writes the project, never the CLI"
    );
}

#[test]
fn new_with_no_directory_leaves_the_folder_to_the_runtime() {
    let working_directory = tempfile::tempdir().unwrap();
    let drawn_project_directory = Path::new("/home/me/streams/brisk-otter");
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_answering_create_project(StubToolAnswer::tool_result(
            &create_project_tool_result_text("Rabc-2", drawn_project_directory),
        ));

    let finished = run_tatolab_in(
        &isolated_machine_directories,
        working_directory.path(),
        &["new"],
    );

    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(
        standard_output_text(&finished),
        "Rabc-2 queued: create of /home/me/streams/brisk-otter\n  follow it: tatolab logs -f \
         --dir /home/me/streams/brisk-otter\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("create_project"),
        [json!({})]
    );
}

#[test]
fn a_refused_create_prints_the_runtimes_refusal_and_exits_one() {
    let working_directory = tempfile::tempdir().unwrap();
    let (isolated_machine_directories, _stub_local_api_server) =
        a_runtime_answering_create_project(StubToolAnswer::tool_failure(
            "no folder for a new project could be created under /home/me/streams in 8 draws",
        ));

    let finished = run_tatolab_in(
        &isolated_machine_directories,
        working_directory.path(),
        &["new"],
    );

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(standard_output_text(&finished), "");
    assert_eq!(
        standard_error_text(&finished),
        "error: create_project failed: no folder for a new project could be created under \
         /home/me/streams in 8 draws\n"
    );
}

#[test]
fn new_with_no_runtime_fails_and_writes_nothing() {
    let working_directory = tempfile::tempdir().unwrap();
    let isolated_machine_directories = IsolatedMachineDirectories::new();

    let finished = run_tatolab_in(
        &isolated_machine_directories,
        working_directory.path(),
        &["new", "probe"],
    );

    assert_ne!(finished.status.code(), Some(0));
    assert!(
        standard_error_text(&finished).starts_with("error: "),
        "{}",
        standard_error_text(&finished)
    );
    assert!(!working_directory.path().join("probe").exists());
}
