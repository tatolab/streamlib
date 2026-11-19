// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's Python node declarations checked in its own interpreter, module by module, by
//! the reader that loads each node, under the node type listing's bound.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::compiler::compiler_ops::entry_run_in_the_projects_interpreter::{
    EntryRunBounds, EntryRunInTheProjectsInterpreter, entry_command_in_the_projects_interpreter,
    run_an_entry_in_the_projects_interpreter,
};
use crate::core::compiler::compiler_ops::project_node_types_listed_from_source::PROJECT_NODE_TYPE_LISTING_BOUND;
use crate::core::runtime::the_projects_venv_interpreter;

/// The `tatolab.stream` module whose `-m` run checks a project's node declarations and prints
/// one line of JSON per module it finishes.
pub const PYTHON_NODE_DECLARATION_CHECK_ENTRY_MODULE: &str =
    "tatolab.stream._project_node_declaration_check_entry";

/// The most of a check's standard output read.
const PYTHON_NODE_DECLARATION_CHECK_STANDARD_OUTPUT_KEPT_BYTES: usize = 64 * 1024 * 1024;

/// The label each line of a check's standard error is logged under.
const PYTHON_NODE_DECLARATION_CHECK_STANDARD_ERROR_LOG_LABEL: &str =
    "python-node-declaration-check";

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PythonNodeDeclarationFindingSeverity {
    /// The declaration is refused as it is.
    Error,
    /// The declaration works and should change.
    Warning,
}

/// One thing the check found in a module, as its entry wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PythonNodeDeclarationFinding {
    /// What was found: the declaration reader's code, `module_does_not_import` or
    /// `declaration_retired`.
    pub finding_code: String,
    /// How much it matters.
    pub severity: PythonNodeDeclarationFindingSeverity,
    /// What was found, with its fix when there is one.
    pub words: String,
    /// The module's path in the project.
    pub path: String,
    /// The line, from 1.
    pub line: Option<u32>,
    /// The column, from 1.
    pub column: Option<u32>,
    /// The node type it is about, `<module>:<name>`.
    pub node_type: Option<String>,
    /// The port it is about, as its declaration names it.
    pub port: Option<String>,
}

/// A module the check finished.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PythonModuleChecked {
    /// Its path in the project.
    pub path: String,
    /// The SHA-256 of the bytes checked, lowercase hex.
    pub content_sha256: String,
    /// What the check found in it.
    pub findings: Vec<PythonNodeDeclarationFinding>,
}

/// Why a check did not run to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonNodeDeclarationCheckNotFinished {
    /// It was still running at its bound and was killed.
    pub ran_out_of_time: bool,
    /// Why, in words.
    pub words: String,
    /// The module it was checking when it ended, by its path in the project.
    pub module_being_checked: Option<String>,
}

/// What a check of a project's node declarations found, and whether it ran to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonNodeDeclarationCheck {
    /// Each program and package it ran with, and its version.
    pub checked_with: Vec<String>,
    /// Each module it finished, in the order it checked them.
    pub modules_checked: Vec<PythonModuleChecked>,
    /// Why it did not run to its end; `None` when it did.
    pub not_finished: Option<PythonNodeDeclarationCheckNotFinished>,
}

/// What the check entry reads on standard input.
#[derive(Serialize)]
struct PythonNodeDeclarationCheckRequest<'request> {
    paths: Option<&'request [String]>,
}

/// The first line the entry prints: the modules it will check, in order.
#[derive(Deserialize)]
struct PythonModulesToCheck {
    modules: Vec<PythonModuleToCheck>,
    checked_with: Vec<String>,
}

#[derive(Deserialize)]
struct PythonModuleToCheck {
    path: String,
}

/// Check the node declarations of the project at `project_directory` in its venv's
/// interpreter: each module its node type listing names, or each of `paths`. Each module's
/// findings are kept as the check finishes it, so a check that runs past the listing's bound,
/// or ends early, keeps what it finished and says which module it was checking.
pub fn check_a_projects_python_node_declarations(
    project_directory: &Path,
    paths: Option<&[String]>,
) -> PythonNodeDeclarationCheck {
    check_python_node_declarations_within(project_directory, paths, PROJECT_NODE_TYPE_LISTING_BOUND)
}

fn check_python_node_declarations_within(
    project_directory: &Path,
    paths: Option<&[String]>,
    bound: Duration,
) -> PythonNodeDeclarationCheck {
    let interpreter_path = the_projects_venv_interpreter(project_directory);
    let interpreter = interpreter_path.display();
    let not_run = |words: String| PythonNodeDeclarationCheck {
        checked_with: Vec::new(),
        modules_checked: Vec::new(),
        not_finished: Some(PythonNodeDeclarationCheckNotFinished {
            ran_out_of_time: false,
            words,
            module_being_checked: None,
        }),
    };
    let request_json = match serde_json::to_vec(&PythonNodeDeclarationCheckRequest { paths }) {
        Ok(request_json) => request_json,
        Err(not_written) => {
            return not_run(format!(
                "the check's request does not write as JSON: {not_written}"
            ));
        }
    };
    let bounds = EntryRunBounds {
        bound,
        standard_output_kept_bytes: PYTHON_NODE_DECLARATION_CHECK_STANDARD_OUTPUT_KEPT_BYTES,
        standard_error_log_label: PYTHON_NODE_DECLARATION_CHECK_STANDARD_ERROR_LOG_LABEL,
        what_runs: "a Python node declaration check",
    };
    let mut command = entry_command_in_the_projects_interpreter(
        &interpreter_path,
        project_directory,
        PYTHON_NODE_DECLARATION_CHECK_ENTRY_MODULE,
    );
    match run_an_entry_in_the_projects_interpreter(&mut command, request_json, &bounds) {
        Ok(run) => the_check_a_run_wrote(&run, bound, &interpreter.to_string()),
        Err(spawn_failure) => not_run(format!(
            "the project's interpreter `{interpreter}` could not be started: {spawn_failure}; \
             the project's environment may not be made yet"
        )),
    }
}

/// The check `run` wrote, line by line: the modules it named, then each it finished.
fn the_check_a_run_wrote(
    run: &EntryRunInTheProjectsInterpreter,
    bound: Duration,
    interpreter: &str,
) -> PythonNodeDeclarationCheck {
    let quoted_standard_error = &run.quoted_standard_error;
    let mut lines = run
        .standard_output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .peekable();
    let mut check = PythonNodeDeclarationCheck {
        checked_with: Vec::new(),
        modules_checked: Vec::new(),
        not_finished: None,
    };
    let mut modules_to_check = Vec::new();
    let mut why_it_did_not_finish = None;
    let mut the_first_line = true;
    while let Some(line) = lines.next() {
        let is_the_last_line = lines.peek().is_none();
        let read = match serde_json::from_slice::<Value>(line) {
            Ok(read) => read,
            // A run killed at its bound may have been killed in the middle of a line.
            Err(_) if run.ran_past_its_bound && is_the_last_line => break,
            Err(not_json) => {
                why_it_did_not_finish = Some(format!(
                    "`{interpreter}` printed a line that is not JSON ({not_json}); its \
                     `tatolab-stream` may differ from the runtime's. {quoted_standard_error}"
                ));
                break;
            }
        };
        if let Some(failed_because) = read.get("failed_because").and_then(Value::as_str) {
            why_it_did_not_finish = Some(format!("the check did not run: {failed_because}"));
            break;
        }
        let read_as_expected = if the_first_line {
            serde_json::from_value::<PythonModulesToCheck>(read).map(|modules| {
                modules_to_check = modules.modules;
                check.checked_with = modules.checked_with;
            })
        } else {
            serde_json::from_value::<PythonModuleChecked>(read)
                .map(|module_checked| check.modules_checked.push(module_checked))
        };
        if let Err(not_read) = read_as_expected {
            why_it_did_not_finish = Some(format!(
                "`{interpreter}` printed a line this runtime does not read ({not_read}); its \
                 `tatolab-stream` may differ from the runtime's. {quoted_standard_error}"
            ));
            break;
        }
        the_first_line = false;
    }
    let module_being_checked = modules_to_check
        .get(check.modules_checked.len())
        .map(|module| module.path.clone());
    if run.ran_past_its_bound {
        check.not_finished = Some(PythonNodeDeclarationCheckNotFinished {
            ran_out_of_time: true,
            words: format!(
                "the check ran past {}s in `{interpreter}` and was killed{}. \
                 {quoted_standard_error}",
                bound.as_secs(),
                module_being_checked
                    .as_deref()
                    .map(|path| format!(" while it imported `{path}`"))
                    .unwrap_or_default()
            ),
            module_being_checked,
        });
        return check;
    }
    let why_it_did_not_finish = why_it_did_not_finish.or_else(|| {
        if the_first_line {
            Some(format!(
                "`{interpreter}` exited with {} and printed no check; its `tatolab-stream` may \
                 predate the declaration check. {quoted_standard_error}",
                run.exit_status_rendered
            ))
        } else {
            module_being_checked.as_deref().map(|path| {
                format!(
                    "`{interpreter}` exited with {} while it imported `{path}`. \
                     {quoted_standard_error}",
                    run.exit_status_rendered
                )
            })
        }
    });
    check.not_finished = why_it_did_not_finish.map(|words| PythonNodeDeclarationCheckNotFinished {
        ran_out_of_time: false,
        words,
        module_being_checked,
    });
    check
}

#[cfg(test)]
mod tests;
