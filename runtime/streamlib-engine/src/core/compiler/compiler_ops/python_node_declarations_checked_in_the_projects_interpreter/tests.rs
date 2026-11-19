// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;

/// A project whose venv's interpreter is a shell script running `script`, standing in for the
/// check entry so the runtime's side is tested without Python.
fn a_project_whose_interpreter_runs(script: &str) -> tempfile::TempDir {
    let project_directory = tempfile::tempdir().unwrap();
    let interpreter = the_projects_venv_interpreter(project_directory.path());
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    std::fs::write(&interpreter, format!("#!/bin/sh\n{script}")).unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
    project_directory
}

const TWO_MODULES_LINE: &str = r#"{"modules":[{"module":"nodes.a","path":"nodes/a.py"},{"module":"nodes.b","path":"nodes/b.py"}],"checked_with":["tatolab-stream 1.0"]}"#;

const MODULE_A_CHECKED_LINE: &str = r#"{"path":"nodes/a.py","content_sha256":"aa","findings":[{"finding_code":"value_input_has_no_default","severity":"error","words":"To fix: give it one.","path":"nodes/a.py","line":4,"column":null,"node_type":"nodes.a:Blur","port":"radius"}]}"#;

const MODULE_B_CHECKED_LINE: &str = r#"{"path":"nodes/b.py","content_sha256":"bb","findings":[]}"#;

fn check_of(project_directory: &Path, bound: Duration) -> PythonNodeDeclarationCheck {
    check_python_node_declarations_within(project_directory, None, bound)
}

#[test]
fn the_check_runs_its_entry_isolated_in_the_projects_venv() {
    let project_directory = tempfile::tempdir().unwrap();

    let command = entry_command_in_the_projects_interpreter(
        &the_projects_venv_interpreter(project_directory.path()),
        project_directory.path(),
        PYTHON_NODE_DECLARATION_CHECK_ENTRY_MODULE,
    );

    let arguments: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        arguments,
        ["-I", "-m", PYTHON_NODE_DECLARATION_CHECK_ENTRY_MODULE]
    );
    assert_eq!(command.get_current_dir(), Some(project_directory.path()));
}

#[test]
fn a_check_that_runs_to_its_end_answers_each_module_and_its_findings() {
    let project_directory = a_project_whose_interpreter_runs(&format!(
        "cat > /dev/null\nprintf '%s\\n' '{TWO_MODULES_LINE}' '{MODULE_A_CHECKED_LINE}' '{MODULE_B_CHECKED_LINE}'\n"
    ));

    let check = check_of(project_directory.path(), Duration::from_secs(30));

    assert_eq!(check.not_finished, None);
    assert_eq!(check.checked_with, ["tatolab-stream 1.0"]);
    assert_eq!(
        check
            .modules_checked
            .iter()
            .map(|module| module.path.as_str())
            .collect::<Vec<_>>(),
        ["nodes/a.py", "nodes/b.py"]
    );
    assert_eq!(
        check.modules_checked[0].findings[0],
        PythonNodeDeclarationFinding {
            finding_code: "value_input_has_no_default".to_owned(),
            severity: PythonNodeDeclarationFindingSeverity::Error,
            words: "To fix: give it one.".to_owned(),
            path: "nodes/a.py".to_owned(),
            line: Some(4),
            column: None,
            node_type: Some("nodes.a:Blur".to_owned()),
            port: Some("radius".to_owned()),
        }
    );
}

#[test]
fn a_check_still_running_at_its_bound_is_killed_keeping_the_modules_it_finished() {
    let project_directory = a_project_whose_interpreter_runs(&format!(
        "cat > /dev/null\nprintf '%s\\n' '{TWO_MODULES_LINE}' '{MODULE_A_CHECKED_LINE}'\nexec sleep 60\n"
    ));

    let check = check_of(project_directory.path(), Duration::from_secs(1));

    assert_eq!(check.modules_checked.len(), 1);
    assert_eq!(check.modules_checked[0].path, "nodes/a.py");
    let not_finished = check.not_finished.unwrap();
    assert!(not_finished.ran_out_of_time);
    assert_eq!(
        not_finished.module_being_checked.as_deref(),
        Some("nodes/b.py")
    );
    assert!(
        not_finished.words.contains("ran past 1s") && not_finished.words.contains("nodes/b.py"),
        "{}",
        not_finished.words
    );
}

#[test]
fn a_check_that_ends_before_its_last_module_names_the_module_it_was_importing() {
    let project_directory = a_project_whose_interpreter_runs(&format!(
        "cat > /dev/null\nprintf '%s\\n' '{TWO_MODULES_LINE}' '{MODULE_A_CHECKED_LINE}'\nexit 3\n"
    ));

    let check = check_of(project_directory.path(), Duration::from_secs(30));

    let not_finished = check.not_finished.unwrap();
    assert!(!not_finished.ran_out_of_time);
    assert_eq!(
        not_finished.module_being_checked.as_deref(),
        Some("nodes/b.py")
    );
    assert_eq!(check.modules_checked.len(), 1);
}

#[test]
fn a_check_whose_entry_could_not_run_says_why() {
    let project_directory = a_project_whose_interpreter_runs(
        "cat > /dev/null\nprintf '%s\\n' '{\"failed_because\": \"Traceback: no listing\"}'\n",
    );

    let check = check_of(project_directory.path(), Duration::from_secs(30));

    let not_finished = check.not_finished.unwrap();
    assert!(
        not_finished.words.contains("Traceback: no listing"),
        "{}",
        not_finished.words
    );
    assert!(check.modules_checked.is_empty());
}

#[test]
fn a_check_whose_interpreter_cannot_start_is_not_run_naming_the_interpreter() {
    let project_directory = tempfile::tempdir().unwrap();

    let check = check_a_projects_python_node_declarations(project_directory.path(), None);

    let not_finished = check.not_finished.unwrap();
    assert!(!not_finished.ran_out_of_time);
    assert!(
        not_finished.words.contains("could not be started"),
        "{}",
        not_finished.words
    );
}
