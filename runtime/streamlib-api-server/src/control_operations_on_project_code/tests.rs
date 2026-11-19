// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::runtime::{PythonModuleChecked, PythonNodeDeclarationCheckNotFinished};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectCodeCheckVerdict;

use super::*;

fn a_warning_in(path: &str) -> PythonNodeDeclarationFinding {
    PythonNodeDeclarationFinding {
        finding_code: "declaration_retired".to_owned(),
        severity: PythonNodeDeclarationFindingSeverity::Warning,
        words: "`@node.function` is retired. To fix: bare `@node`.".to_owned(),
        path: path.to_owned(),
        line: Some(4),
        column: Some(1),
        node_type: Some("nodes.clamp:Clamp".to_owned()),
        port: None,
    }
}

/// The answer of `declaration_check` beside an `extension_manifest` run over a project with no
/// view.
fn the_answer_of_the_declaration_check(
    declaration_check: PythonNodeDeclarationCheck,
) -> ProjectCodeCheckResult {
    the_answer_of_the_checks(
        declaration_check,
        ViewManifestsChecked {
            run: ProjectCodeCheckRun {
                check: ProjectCodeCheckKind::ExtensionManifest,
                state: ProjectCodeCheckState::Passed,
                not_checked_because: None,
                checked_with: Vec::new(),
            },
            findings: Vec::new(),
            files_checked: Vec::new(),
        },
        None,
    )
}

#[test]
fn a_check_killed_at_its_bound_answers_not_checked_with_the_file_it_was_importing() {
    let answer = the_answer_of_the_declaration_check(PythonNodeDeclarationCheck {
        checked_with: vec!["tatolab-stream 1.0".to_owned()],
        modules_checked: vec![PythonModuleChecked {
            path: "nodes/clamp.py".to_owned(),
            content_sha256: "aa".to_owned(),
            findings: vec![a_warning_in("nodes/clamp.py")],
        }],
        not_finished: Some(PythonNodeDeclarationCheckNotFinished {
            ran_out_of_time: true,
            words: "the check ran past 60s".to_owned(),
            module_being_checked: Some("nodes/stuck.py".to_owned()),
        }),
    });

    assert_eq!(answer.verdict, ProjectCodeCheckVerdict::NotFullyChecked);
    let run = &answer.checks[0];
    assert_eq!(run.state, ProjectCodeCheckState::NotChecked);
    let not_checked_because = run.not_checked_because.as_ref().unwrap();
    assert_eq!(
        not_checked_because.refusal_code,
        ControlOperationRefusalCode::CheckRanOutOfTime
    );
    assert_eq!(
        not_checked_because
            .refused_file
            .as_ref()
            .map(|place| place.path.as_str()),
        Some("nodes/stuck.py")
    );
    assert_eq!(
        answer.files_checked,
        [ProjectFileChecked {
            path: "nodes/clamp.py".to_owned(),
            content_sha256: "aa".to_owned(),
        }]
    );
    assert_eq!(answer.findings.len(), 1);
}

#[test]
fn warnings_alone_pass() {
    let answer = the_answer_of_the_declaration_check(PythonNodeDeclarationCheck {
        checked_with: Vec::new(),
        modules_checked: vec![PythonModuleChecked {
            path: "nodes/clamp.py".to_owned(),
            content_sha256: "aa".to_owned(),
            findings: vec![a_warning_in("nodes/clamp.py")],
        }],
        not_finished: None,
    });

    assert_eq!(answer.checks[0].state, ProjectCodeCheckState::Passed);
    assert_eq!(answer.verdict, ProjectCodeCheckVerdict::Passed);
    assert_eq!(
        answer.findings[0].severity,
        ProjectCodeCheckSeverity::Warning
    );
}
