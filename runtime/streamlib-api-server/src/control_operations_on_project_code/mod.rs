// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operation checking a project's code on disk without running a stream: its Python node
//! declarations and its views' manifests.

use streamlib::sdk::runtime::{
    PythonNodeDeclarationCheck, PythonNodeDeclarationFinding, PythonNodeDeclarationFindingSeverity,
    check_a_projects_python_node_declarations,
};
use streamlib_control_contract::CheckProjectCodeRequest;
use streamlib_project_repository::{ViewManifestsChecked, check_a_projects_view_manifests};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ProjectActionRefusalOnTheWire, ProjectCodeCheckFinding,
    ProjectCodeCheckKind, ProjectCodeCheckResult, ProjectCodeCheckRun, ProjectCodeCheckSeverity,
    ProjectCodeCheckState, ProjectFileChecked, ProjectFilePlaceOnTheWire,
};

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};

const CHECK_PROJECT_CODE: &str = "check_project_code";

impl ControlOperationDispatcher {
    pub(crate) async fn check_project_code(
        &self,
        CheckProjectCodeRequest {
            project_directory,
            paths,
        }: CheckProjectCodeRequest,
    ) -> Result<ProjectCodeCheckResult, ControlOperationRefusal> {
        let paths_read = paths.clone();
        let (project_directory, view_manifests_checked) = self
            .a_project_repository_call_off_the_async_runtime(
                CHECK_PROJECT_CODE,
                project_directory,
                move |project_repository| {
                    for path in paths_read.iter().flatten() {
                        project_repository.read_project_file(path)?;
                    }
                    let project_directory = project_repository.project_directory().to_path_buf();
                    let view_manifests_checked =
                        check_a_projects_view_manifests(&project_directory, paths_read.as_deref())?;
                    Ok((project_directory, view_manifests_checked))
                },
            )
            .await?;
        let declaration_check = tokio::task::spawn_blocking(move || {
            check_a_projects_python_node_declarations(&project_directory, paths.as_deref())
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "check_project_code failed: the declaration check",
        ))?;
        Ok(the_answer_of_the_checks(
            declaration_check,
            view_manifests_checked,
        ))
    }
}

/// The answer of a `python_node_declarations` run that ended as `declaration_check` did and an
/// `extension_manifest` run that ended as `view_manifests_checked` did.
fn the_answer_of_the_checks(
    declaration_check: PythonNodeDeclarationCheck,
    view_manifests_checked: ViewManifestsChecked,
) -> ProjectCodeCheckResult {
    let ViewManifestsChecked {
        run: view_manifests_run,
        findings: view_manifest_findings,
        files_checked: view_manifests_read,
    } = view_manifests_checked;
    let (declarations_run, mut findings, mut files_checked) =
        the_run_of_the_declaration_check(declaration_check);
    findings.extend(view_manifest_findings);
    files_checked.extend(view_manifests_read);
    ProjectCodeCheckResult::of(
        vec![declarations_run, view_manifests_run],
        findings,
        files_checked,
    )
}

/// The `python_node_declarations` run that ended as `declaration_check` did, its findings and
/// the files it finished.
fn the_run_of_the_declaration_check(
    declaration_check: PythonNodeDeclarationCheck,
) -> (
    ProjectCodeCheckRun,
    Vec<ProjectCodeCheckFinding>,
    Vec<ProjectFileChecked>,
) {
    let check = ProjectCodeCheckKind::PythonNodeDeclarations;
    let mut findings = Vec::new();
    let mut files_checked = Vec::new();
    for module_checked in declaration_check.modules_checked {
        findings.extend(
            module_checked
                .findings
                .into_iter()
                .map(|finding| a_finding_on_the_wire(check, finding)),
        );
        files_checked.push(ProjectFileChecked {
            path: module_checked.path,
            content_sha256: module_checked.content_sha256,
        });
    }
    let (state, not_checked_because) = match declaration_check.not_finished {
        Some(not_finished) => (
            ProjectCodeCheckState::NotChecked,
            Some(ProjectActionRefusalOnTheWire {
                refused_file: not_finished.module_being_checked.map(|path| {
                    ProjectFilePlaceOnTheWire {
                        path,
                        line: None,
                        column: None,
                    }
                }),
                ..ProjectActionRefusalOnTheWire::of_the_action(
                    CHECK_PROJECT_CODE,
                    if not_finished.ran_out_of_time {
                        ControlOperationRefusalCode::CheckRanOutOfTime
                    } else {
                        ControlOperationRefusalCode::Refused
                    },
                    not_finished.words,
                )
            }),
        ),
        None if findings.iter().any(|finding: &ProjectCodeCheckFinding| {
            finding.severity == ProjectCodeCheckSeverity::Error
        }) =>
        {
            (ProjectCodeCheckState::Failed, None)
        }
        None => (ProjectCodeCheckState::Passed, None),
    };
    (
        ProjectCodeCheckRun {
            check,
            state,
            not_checked_because,
            checked_with: declaration_check.checked_with,
        },
        findings,
        files_checked,
    )
}

fn a_finding_on_the_wire(
    check: ProjectCodeCheckKind,
    finding: PythonNodeDeclarationFinding,
) -> ProjectCodeCheckFinding {
    ProjectCodeCheckFinding {
        check,
        finding_code: finding.finding_code,
        severity: match finding.severity {
            PythonNodeDeclarationFindingSeverity::Error => ProjectCodeCheckSeverity::Error,
            PythonNodeDeclarationFindingSeverity::Warning => ProjectCodeCheckSeverity::Warning,
        },
        words: finding.words,
        path: finding.path,
        line: finding.line,
        column: finding.column,
        node_type: finding.node_type,
        port: finding.port,
    }
}

#[cfg(test)]
mod tests;
