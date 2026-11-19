// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operation checking a project's code on disk without running a stream: its Python node
//! declarations, its views' manifests and node UI manifest, and its views' and controls' code
//! against the extension SDK kit.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use streamlib::sdk::runtime::{
    ExtensionCodeCheck, ExtensionCodeCheckKind, ExtensionCodeCheckRun, ExtensionCodeFinding,
    ExtensionCodeFindingSeverity, ExtensionCodeNotCheckedCode, PythonNodeDeclarationCheck,
    PythonNodeDeclarationFinding, PythonNodeDeclarationFindingSeverity,
    check_a_projects_python_node_declarations,
};
use streamlib_control_contract::CheckProjectCodeRequest;
use streamlib_project_repository::{
    NodeUiManifestToCheck, ViewManifestsChecked, check_a_projects_view_manifests,
    the_findings_of_a_node_ui_manifest, the_node_ui_manifest_a_check_reads,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeTypeSourceOrigin, ProjectActionRefusalOnTheWire,
    ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckResult, ProjectCodeCheckRun,
    ProjectCodeCheckSeverity, ProjectCodeCheckState, ProjectFileChecked, ProjectFilePlaceOnTheWire,
};

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};

const CHECK_PROJECT_CODE: &str = "check_project_code";

/// The suffix of a file the `python_node_declarations` check imports; any other path among
/// `paths` is for another check, and `paths` naming none runs it over nothing.
const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

impl ControlOperationDispatcher {
    pub(crate) async fn check_project_code(
        &self,
        CheckProjectCodeRequest {
            project_directory,
            paths,
        }: CheckProjectCodeRequest,
    ) -> Result<ProjectCodeCheckResult, ControlOperationRefusal> {
        let paths_read = paths.clone();
        let (project_directory, view_manifests_checked, node_ui_manifest) = self
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
                    let node_ui_manifest = the_node_ui_manifest_a_check_reads(
                        &project_directory,
                        paths_read.as_deref(),
                    );
                    Ok((project_directory, view_manifests_checked, node_ui_manifest))
                },
            )
            .await?;
        let node_ui_manifest_checked = match node_ui_manifest {
            Some(to_check) => Some(
                self.the_node_ui_manifest_checked(to_check, project_directory.clone())
                    .await,
            ),
            None => None,
        };
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let extension_project_directory = project_directory.clone();
        let extension_paths = paths.clone();
        let extension_code_check = tokio::task::spawn_blocking(move || {
            operations_on_the_loaded_streams.check_a_projects_extension_code(
                &extension_project_directory,
                extension_paths.as_deref(),
            )
        });
        let module_paths = the_python_module_paths_among(paths);
        let declaration_check = if module_paths.as_ref().is_some_and(Vec::is_empty) {
            PythonNodeDeclarationCheck {
                checked_with: Vec::new(),
                modules_checked: Vec::new(),
                not_finished: None,
            }
        } else {
            tokio::task::spawn_blocking(move || {
                check_a_projects_python_node_declarations(
                    &project_directory,
                    module_paths.as_deref(),
                )
            })
            .await
            .map_err(ControlOperationRefusal::the_task_did_not_finish(
                "check_project_code failed: the declaration check",
            ))?
        };
        let extension_code_check = extension_code_check.await.map_err(
            ControlOperationRefusal::the_task_did_not_finish(
                "check_project_code failed: the extension code check",
            ),
        )?;
        Ok(the_answer_of_the_checks(
            declaration_check,
            view_manifests_checked,
            node_ui_manifest_checked,
            extension_code_check,
        ))
    }

    /// What the `extension_manifest` check finds in the project's own node UI manifest
    /// `to_check`: its node bodies judged against the node types the project's own code declares,
    /// listed only when it registers one.
    async fn the_node_ui_manifest_checked(
        &self,
        to_check: NodeUiManifestToCheck,
        project_directory: PathBuf,
    ) -> NodeUiManifestChecked {
        let node_types_the_project_brings = if to_check.registers_a_node_body() {
            Some(
                self.the_node_types_the_project_offers_read_from_source(
                    CHECK_PROJECT_CODE,
                    project_directory,
                )
                .await
                .map(|listing| {
                    listing
                        .sources
                        .into_iter()
                        .filter(|source| source.origin == NodeTypeSourceOrigin::Project)
                        .flat_map(|source| source.node_types)
                        .map(|read| read.node_type)
                        .collect::<BTreeSet<String>>()
                }),
            )
        } else {
            None
        };
        let (node_types, node_bodies_not_judged_because) = match node_types_the_project_brings {
            Some(Ok(node_types)) => (Some(node_types), None),
            Some(Err(refused)) => (
                None,
                Some(refused.into_the_refusal_on_the_wire(CHECK_PROJECT_CODE)),
            ),
            None => (None, None),
        };
        NodeUiManifestChecked {
            findings: the_findings_of_a_node_ui_manifest(&to_check, node_types.as_ref()),
            file_checked: to_check.file_checked,
            node_bodies_not_judged_because,
        }
    }
}

/// What the `extension_manifest` check found in the project's own node UI manifest.
#[derive(Debug)]
struct NodeUiManifestChecked {
    findings: Vec<ProjectCodeCheckFinding>,
    file_checked: Option<ProjectFileChecked>,
    /// Why its node bodies were not judged: the refusal of the node type listing they are
    /// judged against.
    node_bodies_not_judged_because: Option<ProjectActionRefusalOnTheWire>,
}

/// The paths among `paths` the `python_node_declarations` check imports, or every module of the
/// project when `paths` names none.
fn the_python_module_paths_among(paths: Option<Vec<String>>) -> Option<Vec<String>> {
    paths.map(|paths| {
        paths
            .into_iter()
            .filter(|path| path.ends_with(PYTHON_SOURCE_FILE_SUFFIX))
            .collect()
    })
}

/// The answer of a `python_node_declarations` run that ended as `declaration_check` did, and an
/// `extension_manifest` run over the views as `view_manifests_checked` and over the node UI
/// manifest as `node_ui_manifest_checked`, `None` when the project has none or `paths` leaves it
/// out, and the `extension_types` and `extension_lint` runs of `extension_code_check`.
fn the_answer_of_the_checks(
    declaration_check: PythonNodeDeclarationCheck,
    view_manifests_checked: ViewManifestsChecked,
    node_ui_manifest_checked: Option<NodeUiManifestChecked>,
    extension_code_check: ExtensionCodeCheck,
) -> ProjectCodeCheckResult {
    let ViewManifestsChecked {
        run: mut extension_manifest_run,
        findings: view_manifest_findings,
        files_checked: view_manifests_read,
    } = view_manifests_checked;
    let (declarations_run, mut findings, mut files_checked) =
        the_run_of_the_declaration_check(declaration_check);
    findings.extend(view_manifest_findings);
    files_checked.extend(view_manifests_read);
    if let Some(NodeUiManifestChecked {
        findings: node_ui_manifest_findings,
        file_checked,
        node_bodies_not_judged_because,
    }) = node_ui_manifest_checked
    {
        if node_ui_manifest_findings
            .iter()
            .any(|finding| finding.severity == ProjectCodeCheckSeverity::Error)
        {
            extension_manifest_run.state = ProjectCodeCheckState::Failed;
        }
        if let Some(refusal) = node_bodies_not_judged_because {
            extension_manifest_run.state = ProjectCodeCheckState::NotChecked;
            extension_manifest_run.not_checked_because = Some(refusal);
        }
        findings.extend(node_ui_manifest_findings);
        files_checked.extend(file_checked);
    }
    let ExtensionCodeCheck {
        types,
        lint,
        findings: extension_code_findings,
        files_checked: extension_files_checked,
    } = extension_code_check;
    let extension_code_findings: Vec<ProjectCodeCheckFinding> = extension_code_findings
        .into_iter()
        .map(an_extension_code_finding_on_the_wire)
        .collect();
    let types_run = the_run_of_an_extension_code_check(types, &extension_code_findings);
    let lint_run = the_run_of_an_extension_code_check(lint, &extension_code_findings);
    findings.extend(extension_code_findings);
    files_checked.extend(
        extension_files_checked
            .into_iter()
            .map(|file| ProjectFileChecked {
                path: file.path,
                content_sha256: file.content_sha256,
            }),
    );
    ProjectCodeCheckResult::of(
        vec![
            declarations_run,
            extension_manifest_run,
            types_run,
            lint_run,
        ],
        findings,
        files_checked,
    )
}

/// The check an extension code check's `kind` is on the wire.
fn the_check_of_an_extension_code_kind(kind: ExtensionCodeCheckKind) -> ProjectCodeCheckKind {
    match kind {
        ExtensionCodeCheckKind::Types => ProjectCodeCheckKind::ExtensionTypes,
        ExtensionCodeCheckKind::Lint => ProjectCodeCheckKind::ExtensionLint,
    }
}

/// The `extension_types` or `extension_lint` run that ended as `run` did, failed when one of
/// `findings` of its check is an error.
fn the_run_of_an_extension_code_check(
    run: ExtensionCodeCheckRun,
    findings: &[ProjectCodeCheckFinding],
) -> ProjectCodeCheckRun {
    let check = the_check_of_an_extension_code_kind(run.kind);
    let (state, not_checked_because) = match run.not_checked {
        Some(not_checked) => (
            ProjectCodeCheckState::NotChecked,
            Some(ProjectActionRefusalOnTheWire {
                refused_file: not_checked.folder_being_checked.map(|path| {
                    ProjectFilePlaceOnTheWire {
                        path,
                        line: None,
                        column: None,
                    }
                }),
                ..ProjectActionRefusalOnTheWire::of_the_action(
                    CHECK_PROJECT_CODE,
                    match not_checked.code {
                        ExtensionCodeNotCheckedCode::ExtensionSdkNotFound => {
                            ControlOperationRefusalCode::ExtensionSdkNotFound
                        }
                        ExtensionCodeNotCheckedCode::ProgramNotCarried => {
                            ControlOperationRefusalCode::RuntimeFailed
                        }
                        ExtensionCodeNotCheckedCode::RanOutOfTime => {
                            ControlOperationRefusalCode::CheckRanOutOfTime
                        }
                        ExtensionCodeNotCheckedCode::Refused => {
                            ControlOperationRefusalCode::Refused
                        }
                    },
                    not_checked.words,
                )
            }),
        ),
        None if findings.iter().any(|finding| {
            finding.check == check && finding.severity == ProjectCodeCheckSeverity::Error
        }) =>
        {
            (ProjectCodeCheckState::Failed, None)
        }
        None => (ProjectCodeCheckState::Passed, None),
    };
    ProjectCodeCheckRun {
        check,
        state,
        not_checked_because,
        checked_with: run.checked_with,
    }
}

fn an_extension_code_finding_on_the_wire(finding: ExtensionCodeFinding) -> ProjectCodeCheckFinding {
    ProjectCodeCheckFinding {
        check: the_check_of_an_extension_code_kind(finding.kind),
        finding_code: finding.finding_code,
        severity: match finding.severity {
            ExtensionCodeFindingSeverity::Error => ProjectCodeCheckSeverity::Error,
            ExtensionCodeFindingSeverity::Warning => ProjectCodeCheckSeverity::Warning,
        },
        words: finding.words,
        path: finding.path,
        line: finding.line,
        column: finding.column,
        node_type: None,
        port: None,
    }
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
