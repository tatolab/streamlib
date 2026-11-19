// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's code checked over the socket: its findings answered as data, a project whose
//! interpreter is not made answered not fully checked, and a path that is no file refused.

use std::os::unix::fs::PermissionsExt;

use super::*;
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// Stand the project's venv interpreter in with a shell script printing `lines`, as the check
/// entry prints them, so the operation is driven without a Python environment.
fn the_projects_interpreter_prints(project_directory: &Path, lines: &[&str]) {
    let interpreter = project_directory.join(".venv/bin/python");
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    let quoted_lines: Vec<String> = lines.iter().map(|line| format!("'{line}'")).collect();
    std::fs::write(
        &interpreter,
        format!(
            "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' {}\n",
            quoted_lines.join(" ")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn a_declaration_error_is_answered_with_its_check_place_and_code() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    the_projects_interpreter_prints(
        &served.project_directory(),
        &[
            r#"{"modules":[{"module":"nodes.blur","path":"nodes/blur.py"}],"checked_with":["tatolab-stream 1.0"]}"#,
            r#"{"path":"nodes/blur.py","content_sha256":"ab","findings":[{"finding_code":"value_input_has_no_default","severity":"error","words":"To fix: give it one.","path":"nodes/blur.py","line":4,"column":null,"node_type":"nodes.blur:Blur","port":"radius"}]}"#,
        ],
    );

    let checked = served.answered("check_project_code", json!({})).await;

    assert_eq!(checked["verdict"], "failed");
    assert_eq!(
        checked["checks"],
        json!([{
            "check": "python_node_declarations",
            "state": "failed",
            "checked_with": ["tatolab-stream 1.0"],
        }])
    );
    assert_eq!(
        checked["findings"],
        json!([{
            "check": "python_node_declarations",
            "finding_code": "value_input_has_no_default",
            "severity": "error",
            "words": "To fix: give it one.",
            "path": "nodes/blur.py",
            "line": 4,
            "node_type": "nodes.blur:Blur",
            "port": "radius",
        }])
    );
    assert_eq!(
        checked["files_checked"],
        json!([{ "path": "nodes/blur.py", "content_sha256": "ab" }])
    );
}

#[tokio::test]
async fn a_project_whose_interpreter_is_not_made_is_answered_not_fully_checked() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;

    let checked = served.answered("check_project_code", json!({})).await;

    assert_eq!(checked["verdict"], "not_fully_checked");
    let run = &checked["checks"][0];
    assert_eq!(run["state"], "not_checked");
    assert_eq!(run["not_checked_because"]["refusal_code"], "refused");
    assert!(
        run["not_checked_because"]["runtime_words"]
            .as_str()
            .unwrap()
            .contains("could not be started"),
        "{run}"
    );
}

#[tokio::test]
async fn a_path_that_is_no_file_of_the_project_is_refused_naming_it() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;

    let refusal = crate::mcp::tests::the_refusal_of_a_tool_result(
        &served
            .tool_call(
                "check_project_code",
                json!({ "paths": ["nodes/missing.py"] }),
            )
            .await,
    );

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::FileNotFound
    );
    assert_eq!(
        refusal.refused_file.map(|place| place.path),
        Some("nodes/missing.py".to_owned())
    );
}
