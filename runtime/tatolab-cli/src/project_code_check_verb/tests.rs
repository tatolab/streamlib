// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectCodeCheckKind, ProjectCodeCheckRun, ProjectFileChecked,
};

use super::*;
use crate::stub_local_api_server::{StubLocalApiServer, StubToolAnswer};

fn a_finding(severity: ProjectCodeCheckSeverity, line: Option<u32>) -> ProjectCodeCheckFinding {
    ProjectCodeCheckFinding {
        check: ProjectCodeCheckKind::PythonNodeDeclarations,
        finding_code: "value_input_has_no_default".to_owned(),
        severity,
        words: "To fix: give it one.".to_owned(),
        path: "nodes/blur.py".to_owned(),
        line,
        column: line.map(|_| 1),
        node_type: Some("nodes.blur:Blur".to_owned()),
        port: Some("radius".to_owned()),
    }
}

fn a_check_of(findings: Vec<ProjectCodeCheckFinding>) -> ProjectCodeCheckResult {
    ProjectCodeCheckResult::of(
        vec![ProjectCodeCheckRun {
            check: ProjectCodeCheckKind::PythonNodeDeclarations,
            state: ProjectCodeCheckState::Passed,
            not_checked_because: None,
            checked_with: Vec::new(),
        }],
        findings,
        vec![ProjectFileChecked {
            path: "nodes/blur.py".to_owned(),
            content_sha256: "ab".to_owned(),
        }],
    )
}

fn checked_over_a_stub_answering(
    answer: StubToolAnswer,
    project_directory: &Path,
    paths: &[PathBuf],
) -> (Result<u8, TatolabCommandFailure>, StubLocalApiServer) {
    let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(answer);
    let exit_code = check_the_project_code(
        &stub_local_api_server.local_api_socket_path,
        &ProjectCodeCheckAsked {
            caller_working_directory: project_directory,
            project_directory: None,
            paths,
            answer_as_json: false,
        },
    );
    (exit_code, stub_local_api_server)
}

#[test]
fn each_finding_is_a_line_at_its_place_then_the_verdict() {
    let check = a_check_of(vec![
        a_finding(ProjectCodeCheckSeverity::Error, Some(4)),
        a_finding(ProjectCodeCheckSeverity::Warning, None),
    ]);

    assert_eq!(
        the_check_as_lines(&check),
        "nodes/blur.py:4:1: value_input_has_no_default: To fix: give it one.\n\
         nodes/blur.py: value_input_has_no_default: To fix: give it one.\n\
         failed: 1 error, 1 warning in 1 file checked\n"
    );
}

#[test]
fn the_exit_code_is_zero_one_or_two_for_passed_failed_or_not_fully_checked() {
    let project_directory = tempfile::tempdir().unwrap();
    let project_directory = project_directory.path().canonicalize().unwrap();

    for (check, exit_code) in [
        (a_check_of(Vec::new()), 0),
        (
            a_check_of(vec![a_finding(ProjectCodeCheckSeverity::Error, Some(4))]),
            FAILED_CHECK_EXIT_CODE,
        ),
    ] {
        let (checked, _) = checked_over_a_stub_answering(
            StubToolAnswer::tool_result(&serde_json::to_string(&check).unwrap()),
            &project_directory,
            &[],
        );
        assert_eq!(checked.unwrap(), exit_code);
    }
    assert_eq!(
        the_exit_code_of(ProjectCodeCheckVerdict::NotFullyChecked),
        NOT_FULLY_CHECKED_EXIT_CODE
    );
}

#[test]
fn the_paths_given_are_sent_as_paths_in_the_project() {
    let project_directory = tempfile::tempdir().unwrap();
    let project_directory = project_directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(project_directory.join("nodes")).unwrap();
    std::fs::write(project_directory.join("nodes/blur.py"), "").unwrap();

    let (checked, stub_local_api_server) = checked_over_a_stub_answering(
        StubToolAnswer::tool_result(&serde_json::to_string(&a_check_of(Vec::new())).unwrap()),
        &project_directory,
        &[
            PathBuf::from("nodes/blur.py"),
            PathBuf::from("./nodes/new.py"),
        ],
    );

    assert_eq!(checked.unwrap(), 0);
    let call = &stub_local_api_server.recorded_tool_calls()[0];
    assert_eq!(call.tool_name, CHECK_PROJECT_CODE_OPERATION);
    assert_eq!(
        call.tool_arguments,
        json!({
            "project_directory": project_directory.display().to_string(),
            "paths": ["nodes/blur.py", "nodes/new.py"],
        })
    );
}

#[test]
fn a_path_outside_the_project_is_refused_as_not_checked() {
    let project_directory = tempfile::tempdir().unwrap();
    let project_directory = project_directory.path().canonicalize().unwrap();

    let refused = the_path_in_the_project(
        &project_directory,
        &project_directory,
        Path::new("../elsewhere.py"),
    )
    .unwrap_err();

    assert_eq!(refused.exit_code, NOT_FULLY_CHECKED_EXIT_CODE);
}

#[test]
fn a_refused_check_exits_two_with_the_runtimes_words() {
    let project_directory = tempfile::tempdir().unwrap();
    let project_directory = project_directory.path().canonicalize().unwrap();

    let (checked, _) = checked_over_a_stub_answering(
        StubToolAnswer::tool_failure(
            r#"{"refusal_code":"file_not_found","action":"check_project_code","runtime_words":"no file nodes/missing.py"}"#,
        ),
        &project_directory,
        &[],
    );

    let refused = checked.unwrap_err();
    assert_eq!(refused.exit_code, NOT_FULLY_CHECKED_EXIT_CODE);
    assert_eq!(
        refused.message_for_the_user.as_deref(),
        Some("no file nodes/missing.py")
    );
}
