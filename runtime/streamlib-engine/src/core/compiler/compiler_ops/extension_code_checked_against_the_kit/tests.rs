// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::checker_output_read::{CheckFolderPlaces, compiler_findings, linter_findings};
use super::extension_sdk_kit_found::the_extension_sdk_kit_found;
use super::*;

/// A stand-in kit at `directory`, written at `version`.
fn a_kit_at(directory: &Path, version: &str) {
    std::fs::create_dir_all(directory.join("check_folder")).unwrap();
    std::fs::write(
        directory.join("package.json"),
        format!(r#"{{"name": "@tatolab/tato-app-view-sdk", "version": "{version}"}}"#),
    )
    .unwrap();
    std::fs::write(
        directory.join("check_folder/tsconfig.json"),
        r#"{"include": ["**/*.ts"]}"#,
    )
    .unwrap();
    std::fs::write(
        directory.join("tie_lines.json"),
        r#"{"header": "import type { InputControlElementClass } from \"@tatolab/tato-app-view-sdk/node_ui\";",
            "controls": "import Control{index} from \"./{script}\";\nexport const control{index}: InputControlElementClass<\"{type}\"> = Control{index};"}"#,
    )
    .unwrap();
    std::fs::write(directory.join("oxlint.json"), "{}").unwrap();
}

/// A stand-in program at `path` running `body` under `/bin/sh`, answering `--version` with
/// `version_line`.
fn a_program_at(path: &Path, version_line: &str, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version_line}'; exit 0; fi\n{body}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A project at `directory` whose `node_ui` folder registers two controls, one written in
/// `slider.ts`, and carries an old `.js` file and a `.tsx` file.
fn a_project_with_a_node_ui_folder(directory: &Path) {
    let node_ui = directory.join("node_ui");
    std::fs::create_dir_all(node_ui.join("vendor")).unwrap();
    std::fs::write(
        node_ui.join("tatolab_node_ui.json"),
        r#"{"contract": "1.1", "controls": [
            {"type": "float", "control": "slider", "script": "slider.ts"},
            {"type": "audio", "control": "meter", "script": "meter.ts"}]}"#,
    )
    .unwrap();
    std::fs::write(node_ui.join("slider.ts"), "export default class {}\n").unwrap();
    std::fs::write(node_ui.join("meter.ts"), "export default class {}\n").unwrap();
    std::fs::write(node_ui.join("old.js"), "export {}\n").unwrap();
    std::fs::write(node_ui.join("panel.tsx"), "export {}\n").unwrap();
    std::fs::write(node_ui.join("vendor/lib.js"), "export {}\n").unwrap();
}

/// A compiler that reports one error at the tie line of the second control, with a line of
/// words after it, and one in `slider.ts`, only when the check folder holds the tie file and
/// the kit; each run is counted in `runs`.
fn a_compiler_reporting_errors(path: &Path, runs: &Path) {
    a_program_at(
        path,
        "Version 7.0.2",
        &format!(
            "echo compiler >> '{}'\n\
             if [ ! -f tatolab_node_ui_tie.ts ] || [ ! -f node_modules/@tatolab/tato-app-view-sdk/tie_lines.json ]; then\n\
               echo 'error TS5058: the check folder holds no tie file or kit'; exit 2\n\
             fi\n\
             echo \"tatolab_node_ui_tie.ts(4,14): error TS2322: Type 'typeof Meter' is not assignable.\"\n\
             echo \"  Property 'value' is missing.\"\n\
             echo \"slider.ts(3,7): error TS7006: Parameter 'v' implicitly has an 'any' type.\"\n\
             exit 2",
            runs.display()
        ),
    );
}

/// A linter that reports one broken rule in `slider.ts`.
fn a_linter_reporting_a_rule(path: &Path, runs: &Path) {
    a_program_at(
        path,
        "Version: 1.91.0",
        &format!(
            "echo linter >> '{}'\n\
             echo '{{\"diagnostics\": [{{\"message\": \"Unexpected any.\", \"code\": \"typescript(no-explicit-any)\", \"severity\": \"error\", \"help\": \"Use unknown.\", \"filename\": \"./slider.ts\", \"labels\": [{{\"span\": {{\"line\": 5, \"column\": 3}}}}]}}]}}'\n\
             exit 1",
            runs.display()
        ),
    );
}

fn checkers_of(compiler: &Path, linter: &Path) -> ExtensionCodeCheckersHeld {
    let held = ExtensionCodeCheckersHeld::default();
    held.set(ExtensionCodeCheckers {
        typescript_compiler: Ok(compiler.to_path_buf()),
        linter: Ok(linter.to_path_buf()),
        kit_its_starter_names: None,
    })
    .unwrap();
    held
}

#[test]
fn the_kit_beside_the_lend_is_found_before_the_starters_and_the_projects() {
    let workspace = tempfile::tempdir().unwrap();
    let lend = workspace.path().join("unit/lib/tatolab/lend");
    let starters = workspace.path().join("starters-kit");
    let project = workspace.path().join("project");
    a_kit_at(
        &workspace.path().join("unit/lib/tatolab/extension-sdk"),
        "1.0.0",
    );
    a_kit_at(&starters, "2.0.0");
    a_kit_at(
        &project.join("node_modules/@tatolab/tato-app-view-sdk"),
        "3.0.0",
    );

    let found = |lend: Option<&Path>, starters: Option<&Path>| {
        the_extension_sdk_kit_found(lend, starters, &project)
            .map(|kit| (kit.found_as, kit.sdk_version))
    };

    assert_eq!(
        found(Some(&lend), Some(&starters)),
        Ok(("the runtime unit's", "1.0.0".to_owned()))
    );
    assert_eq!(
        found(None, Some(&starters)),
        Ok(("the starter's", "2.0.0".to_owned()))
    );
    assert_eq!(
        found(None, None),
        Ok(("the project's installed", "3.0.0".to_owned()))
    );
}

#[test]
fn a_kit_missing_a_part_is_passed_over_for_the_next_place() {
    let workspace = tempfile::tempdir().unwrap();
    let starters = workspace.path().join("starters-kit");
    let project = workspace.path().join("project");
    a_kit_at(&starters, "2.0.0");
    std::fs::remove_file(starters.join("tie_lines.json")).unwrap();
    a_kit_at(
        &project.join("node_modules/@tatolab/tato-app-view-sdk"),
        "3.0.0",
    );

    let kit = the_extension_sdk_kit_found(None, Some(&starters), &project).unwrap();

    assert_eq!(kit.sdk_version, "3.0.0");
}

#[test]
fn no_kit_names_each_place_looked_in() {
    let workspace = tempfile::tempdir().unwrap();
    let lend = workspace.path().join("unit/lib/tatolab/lend");
    let starters = workspace.path().join("starters-kit");

    let why = the_extension_sdk_kit_found(Some(&lend), Some(&starters), workspace.path())
        .expect_err("no kit is in any place");

    for looked_in in [
        workspace.path().join("unit/lib/tatolab/extension-sdk"),
        starters,
        workspace
            .path()
            .join("node_modules/@tatolab/tato-app-view-sdk"),
    ] {
        assert!(why.contains(&looked_in.display().to_string()), "{why}");
    }
}

#[test]
fn a_template_is_read_from_the_first_kit_found_or_its_absence_names_the_kit() {
    let workspace = tempfile::tempdir().unwrap();
    let lend = workspace.path().join("unit/lib/tatolab/lend");
    let unit_kit = workspace.path().join("unit/lib/tatolab/extension-sdk");
    let project = workspace.path().join("project");
    a_kit_at(&unit_kit, "1.0.0");
    a_kit_at(
        &project.join("node_modules/@tatolab/tato-app-view-sdk"),
        "3.0.0",
    );
    std::fs::create_dir_all(unit_kit.join("templates")).unwrap();
    std::fs::write(
        unit_kit.join("templates/control.ts"),
        "// \"PIN_TYPE_NAME\"\n",
    )
    .unwrap();
    let held = ExtensionCodeCheckersHeld::default();

    let template = held
        .a_template_from_the_kit(&project, "control.ts", Some(&lend))
        .unwrap();
    assert_eq!(template.template, "// \"PIN_TYPE_NAME\"\n");
    assert!(template.kit.contains("1.0.0"), "{}", template.kit);

    let why = held
        .a_template_from_the_kit(&project, "node_body.ts", Some(&lend))
        .expect_err("the kit holds no node body template");
    assert!(why.contains("templates/node_body.ts"), "{why}");
    assert!(why.contains("1.0.0"), "{why}");

    let why = held
        .a_template_from_the_kit(&project, "control.ts", None)
        .expect_err("the project's kit holds no templates");
    assert!(why.contains("3.0.0"), "{why}");
}

#[test]
fn a_project_with_no_view_or_control_passes_both_checks_without_a_kit() {
    let project = tempfile::tempdir().unwrap();

    let check = ExtensionCodeCheckersHeld::default().check_a_projects_extension_code(
        project.path(),
        None,
        None,
    );

    assert_eq!(check.types.not_checked, None);
    assert_eq!(check.lint.not_checked, None);
    assert!(check.findings.is_empty());
}

#[test]
fn a_view_made_of_js_files_is_refused_naming_each_file_and_ts_without_a_kit() {
    let project = tempfile::tempdir().unwrap();
    let view = project.path().join("views/monitor");
    std::fs::create_dir_all(&view).unwrap();
    std::fs::write(view.join("tatolab_view.json"), "{}").unwrap();
    std::fs::write(view.join("index.js"), "export {}\n").unwrap();

    let check = ExtensionCodeCheckersHeld::default().check_a_projects_extension_code(
        project.path(),
        None,
        None,
    );

    assert_eq!(check.types.not_checked, None);
    assert_eq!(check.lint.not_checked, None);
    assert!(check.types.checked_with.is_empty());
    assert_eq!(
        check
            .findings
            .iter()
            .map(|finding| (finding.finding_code.as_str(), finding.path.as_str()))
            .collect::<Vec<_>>(),
        [("not_written_in_ts", "views/monitor/index.js")]
    );
    let not_ts = &check.findings[0];
    assert_eq!(not_ts.severity, ExtensionCodeFindingSeverity::Error);
    assert_eq!(
        not_ts.words,
        "an extension's own code is written in .ts, so `index.js` is refused: write it as .ts, \
         or put a library the extension carries under vendor/ with its .d.ts beside it"
    );
    assert_eq!(
        check
            .files_checked
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["views/monitor/index.js", "views/monitor/tatolab_view.json"]
    );
}

#[test]
fn no_kit_answers_both_checks_not_checked_naming_the_places() {
    let project = tempfile::tempdir().unwrap();
    a_project_with_a_node_ui_folder(project.path());

    let check = ExtensionCodeCheckersHeld::default().check_a_projects_extension_code(
        project.path(),
        None,
        None,
    );

    for run in [&check.types, &check.lint] {
        let not_checked = run.not_checked.as_ref().expect("no kit is found");
        assert_eq!(
            not_checked.code,
            ExtensionCodeNotCheckedCode::ExtensionSdkNotFound
        );
        assert!(
            not_checked
                .words
                .contains("node_modules/@tatolab/tato-app-view-sdk")
        );
    }
    assert!(check.files_checked.is_empty());
}

#[test]
fn a_kit_with_no_program_answers_the_check_not_checked_for_the_program() {
    let project = tempfile::tempdir().unwrap();
    a_project_with_a_node_ui_folder(project.path());
    a_kit_at(
        &project
            .path()
            .join("node_modules/@tatolab/tato-app-view-sdk"),
        "3.0.0",
    );

    let check = ExtensionCodeCheckersHeld::default().check_a_projects_extension_code(
        project.path(),
        None,
        None,
    );

    assert_eq!(
        check.types.not_checked.map(|not_checked| not_checked.code),
        Some(ExtensionCodeNotCheckedCode::ProgramNotCarried)
    );
}

#[test]
fn a_folder_is_checked_with_its_tie_file_each_finding_placed_in_the_project_and_kept() {
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join("project");
    a_project_with_a_node_ui_folder(&project);
    let lend = workspace.path().join("unit/lib/tatolab/lend");
    a_kit_at(
        &workspace.path().join("unit/lib/tatolab/extension-sdk"),
        "1.0.0",
    );
    let runs = workspace.path().join("runs");
    let compiler = workspace.path().join("unit/bin/typescript/tsc");
    let linter = workspace.path().join("unit/bin/oxlint");
    a_compiler_reporting_errors(&compiler, &runs);
    a_linter_reporting_a_rule(&linter, &runs);
    let held = checkers_of(&compiler, &linter);

    let check = held.check_a_projects_extension_code(&project, None, Some(&lend));

    assert_eq!(check.types.not_checked, None, "{:?}", check.types);
    assert_eq!(check.lint.not_checked, None, "{:?}", check.lint);
    assert_eq!(check.types.checked_with[0], "tsc 7.0.2");
    assert_eq!(check.lint.checked_with[0], "oxlint 1.91.0");
    assert!(check.types.checked_with[1].contains("1.0.0 (the runtime unit's kit"));
    let placed: Vec<_> = check
        .findings
        .iter()
        .map(|finding| {
            (
                finding.kind,
                finding.finding_code.as_str(),
                finding.path.as_str(),
                finding.line,
            )
        })
        .collect();
    assert_eq!(
        placed,
        [
            (
                ExtensionCodeCheckKind::Types,
                "tsx_not_checked",
                "node_ui/panel.tsx",
                None
            ),
            (
                ExtensionCodeCheckKind::Types,
                "TS2322",
                "node_ui/tatolab_node_ui.json",
                None
            ),
            (
                ExtensionCodeCheckKind::Types,
                "TS7006",
                "node_ui/slider.ts",
                Some(3)
            ),
            (
                ExtensionCodeCheckKind::Lint,
                "not_written_in_ts",
                "node_ui/old.js",
                None
            ),
            (
                ExtensionCodeCheckKind::Lint,
                "typescript(no-explicit-any)",
                "node_ui/slider.ts",
                Some(5)
            ),
        ]
    );
    let tie_error = &check.findings[1].words;
    assert!(tie_error.starts_with("`controls[1]` ("), "{tie_error}");
    assert!(tie_error.contains("meter.ts"), "{tie_error}");
    assert!(
        tie_error.ends_with("assignable.\nProperty 'value' is missing."),
        "{tie_error}"
    );
    assert_eq!(check.findings[4].words, "Unexpected any. Use unknown.");
    assert_eq!(
        check
            .files_checked
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [
            "node_ui/meter.ts",
            "node_ui/old.js",
            "node_ui/panel.tsx",
            "node_ui/slider.ts",
            "node_ui/tatolab_node_ui.json",
            "node_ui/vendor/lib.js",
        ]
    );

    let again = held.check_a_projects_extension_code(&project, None, Some(&lend));
    assert_eq!(again, check);
    let runs_after_two_checks = std::fs::read_to_string(&runs).unwrap();
    assert_eq!(
        runs_after_two_checks.lines().count(),
        2,
        "{runs_after_two_checks}"
    );

    std::fs::write(
        project.join("node_ui/slider.ts"),
        "export default class Slider {}\n",
    )
    .unwrap();
    held.check_a_projects_extension_code(&project, None, Some(&lend));
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 4);
}

#[test]
fn paths_reach_only_the_folders_they_name_a_file_of() {
    let project = tempfile::tempdir().unwrap();
    a_project_with_a_node_ui_folder(project.path());
    std::fs::create_dir_all(project.path().join("views/meter")).unwrap();
    std::fs::write(project.path().join("views/meter/tatolab_view.json"), "{}").unwrap();
    std::fs::create_dir_all(project.path().join("views/no_manifest")).unwrap();

    assert_eq!(
        the_extension_folders_to_check(project.path(), None),
        ["node_ui", "views/meter"]
    );
    assert_eq!(
        the_extension_folders_to_check(
            project.path(),
            Some(&[
                "views/meter/meter.ts".to_owned(),
                "nodes/clamp.py".to_owned()
            ])
        ),
        ["views/meter"]
    );
    assert!(
        the_extension_folders_to_check(project.path(), Some(&["node_ui_other/a.ts".to_owned()]))
            .is_empty()
    );
}

#[test]
fn a_compiler_that_reports_no_error_and_fails_answers_refused() {
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join("project");
    a_project_with_a_node_ui_folder(&project);
    a_kit_at(
        &project.join("node_modules/@tatolab/tato-app-view-sdk"),
        "3.0.0",
    );
    let runs = workspace.path().join("runs");
    let compiler = workspace.path().join("tsc");
    let linter = workspace.path().join("oxlint");
    a_program_at(
        &compiler,
        "Version 7.0.2",
        "echo 'out of memory' >&2; exit 134",
    );
    a_linter_reporting_a_rule(&linter, &runs);

    let check =
        checkers_of(&compiler, &linter).check_a_projects_extension_code(&project, None, None);

    let not_checked = check.types.not_checked.expect("the compiler crashed");
    assert_eq!(not_checked.code, ExtensionCodeNotCheckedCode::Refused);
    assert_eq!(not_checked.folder_being_checked.as_deref(), Some("node_ui"));
    assert!(
        not_checked.words.contains("out of memory"),
        "{}",
        not_checked.words
    );
    assert_eq!(check.lint.not_checked, None);
    assert!(check.files_checked.is_empty());
}

fn places_with_two_controls(controls: &[String]) -> CheckFolderPlaces<'_> {
    CheckFolderPlaces {
        folder_path: "node_ui",
        manifest_path: "node_ui/tatolab_node_ui.json",
        tie_header_line_count: 1,
        tie_lines_per_control: 2,
        tied_controls: controls,
    }
}

#[test]
fn a_compiler_line_at_no_file_and_one_in_the_kit_are_the_folders() {
    let controls = vec!["slider".to_owned(), "meter".to_owned()];
    let findings = compiler_findings(
        "error TS5083: Cannot read file 'tsconfig.json'.\n\
         node_modules/@tatolab/tato-app-view-sdk/node_ui.d.ts(9,1): error TS2300: Duplicate identifier.\n\
         tatolab_node_ui_tie.ts(2,1): error TS2307: Cannot find module './slider.ts'.\n",
        &places_with_two_controls(&controls),
    );

    assert_eq!(
        findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line, finding.words.as_str()))
            .collect::<Vec<_>>(),
        [
            ("node_ui", None, "Cannot read file 'tsconfig.json'."),
            (
                "node_ui",
                None,
                "node_modules/@tatolab/tato-app-view-sdk/node_ui.d.ts: Duplicate identifier."
            ),
            (
                "node_ui/tatolab_node_ui.json",
                None,
                "`controls[0]` (slider): Cannot find module './slider.ts'."
            ),
        ]
    );
}

#[test]
fn a_linter_that_prints_no_report_says_so() {
    let controls = Vec::new();

    let why = linter_findings(
        "Failed to parse config",
        &places_with_two_controls(&controls),
    )
    .expect_err("no JSON report");

    assert!(why.contains("no JSON report"), "{why}");
}
