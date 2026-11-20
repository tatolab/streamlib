// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::runtime::{
    ExtensionCodeNotChecked, ExtensionFileChecked, PythonModuleChecked,
    PythonNodeDeclarationCheckNotFinished,
};
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
        an_extension_code_check(None, Vec::new(), Vec::new()),
    )
}

/// An extension code check whose two runs did not run to their end as `not_checked` says, or
/// ran with the kit, that found `findings` and finished `files_checked`.
fn an_extension_code_check(
    not_checked: Option<ExtensionCodeNotChecked>,
    findings: Vec<ExtensionCodeFinding>,
    files_checked: Vec<ExtensionFileChecked>,
) -> ExtensionCodeCheck {
    let run = |kind| ExtensionCodeCheckRun {
        kind,
        not_checked: not_checked.clone(),
        checked_with: vec!["tsc 7.0.2".to_owned()],
    };
    ExtensionCodeCheck {
        types: run(ExtensionCodeCheckKind::Types),
        lint: run(ExtensionCodeCheckKind::Lint),
        findings,
        files_checked,
    }
}

/// The answer of a project with no Python node and no view, beside `extension_code_check`.
fn the_answer_of_the_extension_code_check(
    extension_code_check: ExtensionCodeCheck,
) -> ProjectCodeCheckResult {
    the_answer_of_the_checks(
        PythonNodeDeclarationCheck {
            checked_with: Vec::new(),
            modules_checked: Vec::new(),
            not_finished: None,
        },
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
        extension_code_check,
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

#[test]
fn no_extension_sdk_kit_answers_both_extension_checks_not_checked_with_its_code() {
    let answer = the_answer_of_the_extension_code_check(an_extension_code_check(
        Some(ExtensionCodeNotChecked {
            code: ExtensionCodeNotCheckedCode::ExtensionSdkNotFound,
            words: "the runtime found no extension SDK kit".to_owned(),
            folder_being_checked: None,
        }),
        Vec::new(),
        Vec::new(),
    ));

    assert_eq!(answer.verdict, ProjectCodeCheckVerdict::NotFullyChecked);
    assert_eq!(answer.checks[1].state, ProjectCodeCheckState::Passed);
    for (run, check) in answer.checks[2..].iter().zip([
        ProjectCodeCheckKind::ExtensionTypes,
        ProjectCodeCheckKind::ExtensionLint,
    ]) {
        assert_eq!(run.check, check);
        assert_eq!(run.state, ProjectCodeCheckState::NotChecked);
        assert_eq!(
            run.not_checked_because
                .as_ref()
                .map(|refusal| refusal.refusal_code),
            Some(ControlOperationRefusalCode::ExtensionSdkNotFound)
        );
    }
}

#[test]
fn a_type_error_fails_extension_types_alone_and_lists_the_files_checked() {
    let answer = the_answer_of_the_extension_code_check(an_extension_code_check(
        None,
        vec![
            ExtensionCodeFinding {
                kind: ExtensionCodeCheckKind::Types,
                finding_code: "TS2322".to_owned(),
                severity: ExtensionCodeFindingSeverity::Error,
                words: "Type 'string' is not assignable to type 'number'.".to_owned(),
                path: "views/meter/meter.ts".to_owned(),
                line: Some(3),
                column: Some(7),
            },
            ExtensionCodeFinding {
                kind: ExtensionCodeCheckKind::Lint,
                finding_code: "not_written_in_ts".to_owned(),
                severity: ExtensionCodeFindingSeverity::Warning,
                words: "an extension's code is written in .ts".to_owned(),
                path: "views/meter/old.js".to_owned(),
                line: None,
                column: None,
            },
        ],
        vec![ExtensionFileChecked {
            path: "views/meter/meter.ts".to_owned(),
            content_sha256: "bb".to_owned(),
        }],
    ));

    assert_eq!(answer.verdict, ProjectCodeCheckVerdict::Failed);
    assert_eq!(answer.checks[2].check, ProjectCodeCheckKind::ExtensionTypes);
    assert_eq!(answer.checks[2].state, ProjectCodeCheckState::Failed);
    assert_eq!(answer.checks[2].checked_with, ["tsc 7.0.2"]);
    assert_eq!(answer.checks[3].state, ProjectCodeCheckState::Passed);
    assert_eq!(
        answer
            .findings
            .iter()
            .map(|finding| (finding.check, finding.finding_code.as_str()))
            .collect::<Vec<_>>(),
        [
            (ProjectCodeCheckKind::ExtensionTypes, "TS2322"),
            (ProjectCodeCheckKind::ExtensionLint, "not_written_in_ts"),
        ]
    );
    assert_eq!(
        answer.files_checked,
        [ProjectFileChecked {
            path: "views/meter/meter.ts".to_owned(),
            content_sha256: "bb".to_owned(),
        }]
    );
}

#[test]
fn the_declaration_check_imports_only_the_python_files_among_the_paths() {
    assert_eq!(
        the_python_module_paths_among(Some(vec![
            "views/meter/main.ts".to_owned(),
            "nodes/clamp.py".to_owned(),
            "views/meter/tato-view.json".to_owned(),
        ])),
        Some(vec!["nodes/clamp.py".to_owned()]),
    );
    assert_eq!(
        the_python_module_paths_among(Some(vec!["views/meter/main.ts".to_owned()])),
        Some(Vec::new()),
    );
    assert_eq!(the_python_module_paths_among(None), None);
}
