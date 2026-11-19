// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab check`: a project's code checked by the machine's runtime without running a stream,
//! each finding printed at its place.

use std::path::{Component, Path, PathBuf};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectActionRefusalOnTheWire, ProjectCodeCheckFinding, ProjectCodeCheckResult,
    ProjectCodeCheckSeverity, ProjectCodeCheckState, ProjectCodeCheckVerdict,
};

use crate::TatolabCommandFailure;
use crate::local_api_connection::LocalApiConnection;
use crate::local_api_mcp_tool_client::{OperationAnswered, PROJECT_CODE_CHECK_TIMEOUT};
use crate::machine_runtime_local_api_socket::local_api_socket_of_the_running_runtime;
use crate::stream_actions_on_the_runtime::the_project_directory_a_verb_names;
use crate::verb_standard_output::write_verb_standard_output;

/// The operation `tatolab check` calls.
const CHECK_PROJECT_CODE_OPERATION: &str = "check_project_code";

/// The exit code of a check that found an error.
const FAILED_CHECK_EXIT_CODE: u8 = 1;

/// The exit code of a check that did not run to its end, or did not run at all.
const NOT_FULLY_CHECKED_EXIT_CODE: u8 = 2;

/// What `tatolab check` was asked to check.
pub(crate) struct ProjectCodeCheckAsked<'asked> {
    /// The working directory the verb runs in, which `paths` and `project_directory` are read
    /// against.
    pub(crate) caller_working_directory: &'asked Path,
    /// `--dir`, when given.
    pub(crate) project_directory: Option<&'asked Path>,
    /// Each `PATH` given.
    pub(crate) paths: &'asked [PathBuf],
    /// `--json`: print the runtime's answer whole.
    pub(crate) answer_as_json: bool,
}

/// Check the code of the project `asked` names on the machine's running runtime and print what
/// it found: exit 0 when it passed, 1 when it failed and 2 when it was not fully checked.
pub(crate) fn check_the_project_code_on_the_running_runtime(
    asked: &ProjectCodeCheckAsked<'_>,
) -> Result<u8, TatolabCommandFailure> {
    let local_api_socket_path =
        local_api_socket_of_the_running_runtime().map_err(as_not_fully_checked)?;
    check_the_project_code(&local_api_socket_path, asked)
}

fn check_the_project_code(
    local_api_socket_path: &Path,
    asked: &ProjectCodeCheckAsked<'_>,
) -> Result<u8, TatolabCommandFailure> {
    let project_directory =
        the_project_directory_a_verb_names(asked.caller_working_directory, asked.project_directory)
            .map_err(as_not_fully_checked)?;
    let mut params = serde_json::Map::new();
    params.insert(
        "project_directory".to_owned(),
        serde_json::Value::String(project_directory.display().to_string()),
    );
    if !asked.paths.is_empty() {
        let paths = asked
            .paths
            .iter()
            .map(|path| {
                the_path_in_the_project(asked.caller_working_directory, &project_directory, path)
                    .map(serde_json::Value::String)
            })
            .collect::<Result<Vec<_>, _>>()?;
        params.insert("paths".to_owned(), serde_json::Value::Array(paths));
    }
    let mut local_api_connection =
        LocalApiConnection::open(local_api_socket_path, PROJECT_CODE_CHECK_TIMEOUT)
            .map_err(|failure| as_not_fully_checked(failure.into()))?;
    let answer = match local_api_connection
        .call_operation_by_name(CHECK_PROJECT_CODE_OPERATION, params)
        .map_err(|failure| as_not_fully_checked(failure.into()))?
    {
        OperationAnswered::Answered(answer) => answer,
        OperationAnswered::Refused(refusal) => {
            return Err(as_not_fully_checked(TatolabCommandFailure::refused(
                the_words_of_a_refusal(&refusal),
            )));
        }
    };
    let check: ProjectCodeCheckResult = serde_json::from_str(&answer).map_err(|not_read| {
        as_not_fully_checked(TatolabCommandFailure::refused(format!(
            "the runtime answered a check this `tatolab` does not read ({not_read}); it may be \
             older or newer than the runtime"
        )))
    })?;
    if asked.answer_as_json {
        write_verb_standard_output(&format!("{answer}\n"))?;
    } else {
        write_verb_standard_output(&the_check_as_lines(&check))?;
    }
    Ok(the_exit_code_of(check.verdict))
}

/// `path`, read against `caller_working_directory`, as the `/`-separated path inside
/// `project_directory` the runtime takes; refused when it lies outside the project.
fn the_path_in_the_project(
    caller_working_directory: &Path,
    project_directory: &Path,
    path: &Path,
) -> Result<String, TatolabCommandFailure> {
    let joined = caller_working_directory.join(path);
    let resolved = joined
        .canonicalize()
        .unwrap_or_else(|_| lexically_normal(&joined));
    let inside = resolved.strip_prefix(project_directory).map_err(|_| {
        as_not_fully_checked(TatolabCommandFailure::refused(format!(
            "{} is outside the project {}",
            path.display(),
            project_directory.display()
        )))
    })?;
    Ok(inside
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// `path` with each `.` dropped and each `..` taking the component before it, as a path that
/// does not exist yet cannot be canonicalized.
fn lexically_normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// What a check found, one line per finding, `path:line:column: finding_code: words`, then a
/// line for each check that did not run to its end and a closing line with the verdict.
fn the_check_as_lines(check: &ProjectCodeCheckResult) -> String {
    let mut lines: Vec<String> = check.findings.iter().map(the_line_of_a_finding).collect();
    for run in &check.checks {
        if run.state != ProjectCodeCheckState::NotChecked {
            continue;
        }
        let check_name = serde_json::to_value(run.check)
            .ok()
            .and_then(|check_name| check_name.as_str().map(str::to_owned))
            .unwrap_or_default();
        lines.push(match &run.not_checked_because {
            Some(why) => format!(
                "{check_name}: not checked: {}: {}",
                the_refusal_code_of(why),
                why.runtime_words
            ),
            None => format!("{check_name}: not checked"),
        });
    }
    let count_of = |severity| {
        check
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    let errors = count_of(ProjectCodeCheckSeverity::Error);
    let warnings = count_of(ProjectCodeCheckSeverity::Warning);
    let files = check.files_checked.len();
    lines.push(format!(
        "{}: {errors} {}, {warnings} {} in {files} {} checked",
        match check.verdict {
            ProjectCodeCheckVerdict::Passed => "passed",
            ProjectCodeCheckVerdict::Failed => "failed",
            ProjectCodeCheckVerdict::NotFullyChecked => "not fully checked",
        },
        if errors == 1 { "error" } else { "errors" },
        if warnings == 1 { "warning" } else { "warnings" },
        if files == 1 { "file" } else { "files" },
    ));
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// `path:line:column: finding_code: words`, the line and column left out when the finding has
/// none.
fn the_line_of_a_finding(finding: &ProjectCodeCheckFinding) -> String {
    let place = [
        Some(finding.path.clone()),
        finding.line.map(|line| line.to_string()),
        finding
            .line
            .and(finding.column)
            .map(|column| column.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(":");
    format!("{place}: {}: {}", finding.finding_code, finding.words)
}

fn the_refusal_code_of(refusal: &ProjectActionRefusalOnTheWire) -> String {
    serde_json::to_value(refusal.refusal_code)
        .ok()
        .and_then(|code| code.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A refusal's words, read from its JSON; the JSON itself when it does not read.
fn the_words_of_a_refusal(refusal: &str) -> String {
    serde_json::from_str::<ProjectActionRefusalOnTheWire>(refusal)
        .map(|refusal| refusal.runtime_words)
        .unwrap_or_else(|_| refusal.to_owned())
}

fn the_exit_code_of(verdict: ProjectCodeCheckVerdict) -> u8 {
    match verdict {
        ProjectCodeCheckVerdict::Passed => 0,
        ProjectCodeCheckVerdict::Failed => FAILED_CHECK_EXIT_CODE,
        ProjectCodeCheckVerdict::NotFullyChecked => NOT_FULLY_CHECKED_EXIT_CODE,
    }
}

/// `failure` exiting 2: the check did not run, so nothing was checked.
fn as_not_fully_checked(failure: TatolabCommandFailure) -> TatolabCommandFailure {
    TatolabCommandFailure {
        exit_code: NOT_FULLY_CHECKED_EXIT_CODE,
        ..failure
    }
}

#[cfg(test)]
mod tests;
