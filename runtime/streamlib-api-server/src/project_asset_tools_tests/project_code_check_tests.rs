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
        json!([
            {
                "check": "python_node_declarations",
                "state": "failed",
                "checked_with": ["tatolab-stream 1.0"],
            },
            { "check": "extension_manifest", "state": "passed" },
        ])
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
async fn a_views_port_declared_by_family_is_a_warning_naming_the_pin_type_and_the_verdict_still_passes()
 {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    the_projects_interpreter_prints(
        &served.project_directory(),
        &[r#"{"modules":[],"checked_with":["tatolab-stream 1.0"]}"#],
    );
    let view_folder = served.project_directory().join("views/monitor");
    std::fs::create_dir_all(&view_folder).unwrap();
    std::fs::write(
        view_folder.join("tatolab_view.json"),
        r#"{"contract": "1.0", "title": "Monitor", "entry": "index.html", "placement": "node",
            "inputs": [{"name": "picture", "family": "image"}]}"#,
    )
    .unwrap();

    let checked = served.answered("check_project_code", json!({})).await;

    assert_eq!(checked["verdict"], "passed", "{checked}");
    assert_eq!(
        checked["checks"][1],
        json!({ "check": "extension_manifest", "state": "passed" })
    );
    let finding = &checked["findings"][0];
    assert_eq!(finding["check"], "extension_manifest");
    assert_eq!(finding["finding_code"], "port_declared_by_family");
    assert_eq!(finding["severity"], "warning");
    assert_eq!(finding["path"], "views/monitor/tatolab_view.json");
    assert_eq!(finding["port"], "picture");
    assert!(
        finding["words"]
            .as_str()
            .unwrap()
            .contains(r#"write `"pin_type": {"type": "texture"}`"#),
        "{finding}"
    );
    assert_eq!(
        checked["files_checked"][0]["path"],
        "views/monitor/tatolab_view.json"
    );
}

#[tokio::test]
async fn a_views_manifest_among_the_paths_is_read_by_the_manifest_check_alone() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let interpreter = served.project_directory().join(".venv/bin/python");
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    std::fs::write(
        &interpreter,
        "#!/bin/sh\n\
         if grep -q tatolab_view.json; then echo 'the request named a view manifest' >&2; exit 3; fi\n\
         printf '%s\\n' '{\"modules\":[],\"checked_with\":[\"tatolab-stream 1.0\"]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let view_folder = served.project_directory().join("views/monitor");
    std::fs::create_dir_all(&view_folder).unwrap();
    std::fs::write(
        view_folder.join("tatolab_view.json"),
        r#"{"contract": "1.1", "title": "Monitor", "entry": "index.html", "placement": "node",
            "inputs": [{"name": "picture", "pin_type": {"type": "texture"}}]}"#,
    )
    .unwrap();

    let checked = served
        .answered(
            "check_project_code",
            json!({ "paths": ["views/monitor/tatolab_view.json"] }),
        )
        .await;

    assert_eq!(checked["verdict"], "passed", "{checked}");
    assert_eq!(checked["findings"], json!([]), "{checked}");
    assert_eq!(
        checked["files_checked"],
        json!([{ "path": "views/monitor/tatolab_view.json",
                 "content_sha256": checked["files_checked"][0]["content_sha256"] }])
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

#[tokio::test]
async fn a_node_ui_manifest_that_registers_floats_default_control_fails_the_check_naming_the_entry()
{
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    the_projects_interpreter_prints(
        &served.project_directory(),
        &[r#"{"modules":[],"checked_with":["tatolab-stream 1.0"]}"#],
    );
    let node_ui_folder = served.project_directory().join("node_ui");
    std::fs::create_dir_all(&node_ui_folder).unwrap();
    std::fs::write(
        node_ui_folder.join("tatolab_node_ui.json"),
        r#"{"contract": "1.1", "controls": [
            {"type": "float", "control": "knob", "script": "knob.ts"},
            {"type": "float", "script": "float_number.ts"}]}"#,
    )
    .unwrap();

    let checked = served.answered("check_project_code", json!({})).await;

    assert_eq!(checked["verdict"], "failed", "{checked}");
    assert_eq!(
        checked["checks"][1],
        json!({ "check": "extension_manifest", "state": "failed" })
    );
    let [finding] = checked["findings"].as_array().unwrap().as_slice() else {
        panic!("one finding: {checked}");
    };
    assert_eq!(
        finding["finding_code"],
        "default_control_not_the_modules_to_register"
    );
    assert_eq!(finding["severity"], "error");
    assert_eq!(finding["path"], "node_ui/tatolab_node_ui.json");
    let words = finding["words"].as_str().unwrap();
    assert!(
        words.contains("`controls[1]`")
            && words.contains("`float`")
            && words.contains("only the Studio's own module may register it"),
        "{words}"
    );
    assert_eq!(
        checked["files_checked"][0]["path"],
        "node_ui/tatolab_node_ui.json"
    );
}

#[tokio::test]
async fn a_node_body_whose_node_types_do_not_list_is_not_judged_and_the_check_is_not_finished() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    the_projects_interpreter_prints(
        &served.project_directory(),
        &[r#"{"modules":[],"checked_with":["tatolab-stream 1.0"]}"#],
    );
    let node_ui_folder = served.project_directory().join("node_ui");
    std::fs::create_dir_all(&node_ui_folder).unwrap();
    std::fs::write(
        node_ui_folder.join("tatolab_node_ui.json"),
        r#"{"contract": "1.1",
            "node_bodies": [{"node_type": "crossfade:Crossfade", "script": "crossfade.ts"}]}"#,
    )
    .unwrap();

    let checked = served
        .answered(
            "check_project_code",
            json!({ "paths": ["node_ui/tatolab_node_ui.json"] }),
        )
        .await;

    assert_eq!(checked["verdict"], "not_fully_checked", "{checked}");
    let run = &checked["checks"][1];
    assert_eq!(run["check"], "extension_manifest");
    assert_eq!(run["state"], "not_checked");
    assert!(
        run["not_checked_because"]["runtime_words"].is_string(),
        "{run}"
    );
    assert_eq!(checked["findings"], json!([]), "{checked}");
}

#[tokio::test]
async fn a_node_body_for_a_node_type_the_projects_own_code_does_not_declare_fails_the_check() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let interpreter = served.project_directory().join(".venv/bin/python");
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    std::fs::write(
        &interpreter,
        r#"#!/bin/sh
cat > /dev/null
case "$*" in
  *_project_node_type_listing_entry*) printf '%s\n' '{"sources":[{"source":"project","node_types":[{"type":"crossfade:Crossfade","kind":"process","description":"","category":null,"inputs":[],"outputs":[],"path":"crossfade.py","line":3}],"graph_generators":[],"unreadable_modules":[]}]}' ;;
  *) printf '%s\n' '{"modules":[],"checked_with":["tatolab-stream 1.0"]}' ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let node_ui_folder = served.project_directory().join("node_ui");
    std::fs::create_dir_all(&node_ui_folder).unwrap();
    std::fs::write(
        node_ui_folder.join("tatolab_node_ui.json"),
        r#"{"contract": "1.1", "node_bodies": [
            {"node_type": "crossfade:Crossfade", "script": "crossfade.ts"},
            {"node_type": "elsewhere:Mixer", "script": "mixer.ts"}]}"#,
    )
    .unwrap();

    let checked = served.answered("check_project_code", json!({})).await;

    assert_eq!(checked["verdict"], "failed", "{checked}");
    let [finding] = checked["findings"].as_array().unwrap().as_slice() else {
        panic!("one finding: {checked}");
    };
    assert_eq!(
        finding["finding_code"],
        "node_body_not_the_modules_to_register"
    );
    assert_eq!(finding["node_type"], "elsewhere:Mixer");
    assert!(
        finding["words"]
            .as_str()
            .unwrap()
            .contains("`node_bodies[1]`"),
        "{finding}"
    );
}
