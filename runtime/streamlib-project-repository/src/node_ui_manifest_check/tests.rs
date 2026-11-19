// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_project_holding_the_node_ui_manifest(manifest: &str) -> tempfile::TempDir {
    let project_directory = tempfile::tempdir().unwrap();
    let folder = project_directory.path().join(NODE_UI_DIRECTORY);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(NODE_UI_MANIFEST_FILE_NAME), manifest).unwrap();
    project_directory
}

const MANIFEST: &str = r#"{"contract": "1.1",
  "controls": [
    {"type": "float", "script": "float_number.ts"},
    {"type": "float", "control": "knob", "script": "knob.ts"}
  ],
  "node_bodies": [
    {"node_type": "crossfade:Crossfade", "script": "crossfade.ts"},
    {"node_type": "elsewhere:Mixer", "script": "mixer.ts"}
  ]}"#;

fn the_codes_and_words(findings: &[ProjectCodeCheckFinding]) -> Vec<(&str, &str)> {
    findings
        .iter()
        .map(|finding| (finding.finding_code.as_str(), finding.words.as_str()))
        .collect()
}

#[test]
fn a_built_in_types_default_control_and_a_body_for_a_node_type_the_project_does_not_bring_are_errors()
 {
    let project_directory = a_project_holding_the_node_ui_manifest(MANIFEST);
    let to_check = the_node_ui_manifest_a_check_reads(project_directory.path(), None)
        .expect("the project has a node UI manifest");
    assert!(to_check.registers_a_node_body());
    assert_eq!(
        to_check.file_checked,
        Some(ProjectFileChecked {
            path: "node_ui/tatolab_node_ui.json".to_owned(),
            content_sha256: content_sha256_of(MANIFEST.as_bytes()),
        })
    );

    let node_types = BTreeSet::from(["crossfade:Crossfade".to_owned()]);
    let findings = the_findings_of_a_node_ui_manifest(&to_check, Some(&node_types));

    let codes_and_words = the_codes_and_words(&findings);
    let [(default_code, default_words), (body_code, body_words)] = codes_and_words.as_slice()
    else {
        panic!("two findings: {findings:?}");
    };
    assert_eq!(*default_code, "default_control_not_the_modules_to_register");
    assert!(
        default_words.contains("`controls[0]` registers the default control of `float`")
            && default_words.contains("only the Studio's own module may register it"),
        "{default_words}"
    );
    assert_eq!(*body_code, "node_body_not_the_modules_to_register");
    assert!(
        body_words.contains("`node_bodies[1]`") && body_words.contains("`elsewhere:Mixer`"),
        "{body_words}"
    );
    assert_eq!(findings[1].node_type.as_deref(), Some("elsewhere:Mixer"));
    assert!(findings.iter().all(|finding| {
        finding.severity == ProjectCodeCheckSeverity::Error
            && finding.check == ProjectCodeCheckKind::ExtensionManifest
            && finding.path == "node_ui/tatolab_node_ui.json"
    }));

    let without_the_node_types = the_findings_of_a_node_ui_manifest(&to_check, None);
    assert_eq!(
        the_codes_and_words(&without_the_node_types)
            .into_iter()
            .map(|(code, _)| code)
            .collect::<Vec<_>>(),
        ["default_control_not_the_modules_to_register"],
        "a node body is not judged without the node types"
    );
}

#[test]
fn a_manifest_that_does_not_read_is_an_error_and_one_outside_the_paths_is_not_read() {
    let project_directory = a_project_holding_the_node_ui_manifest(r#"{"contract": "2.0"}"#);
    let to_check = the_node_ui_manifest_a_check_reads(project_directory.path(), None)
        .expect("the project has a node UI manifest");
    assert!(!to_check.registers_a_node_body());

    let findings = the_findings_of_a_node_ui_manifest(&to_check, None);

    let [refused] = findings.as_slice() else {
        panic!("one finding: {findings:?}");
    };
    assert_eq!(refused.finding_code, "node_ui_manifest_refused");
    assert!(refused.words.contains("`contract`"), "{}", refused.words);
    assert_eq!(
        the_node_ui_manifest_a_check_reads(
            project_directory.path(),
            Some(&["nodes/blur.py".to_owned()])
        ),
        None
    );
    assert!(
        the_node_ui_manifest_a_check_reads(
            project_directory.path(),
            Some(&["node_ui/tatolab_node_ui.json".to_owned()])
        )
        .is_some()
    );
}

#[test]
fn a_project_with_no_node_ui_manifest_has_nothing_to_check() {
    let project_directory = tempfile::tempdir().unwrap();
    assert_eq!(
        the_node_ui_manifest_a_check_reads(project_directory.path(), None),
        None
    );
    assert!(is_a_projects_own_node_ui_manifest(
        "node_ui/tatolab_node_ui.json"
    ));
    assert!(!is_a_projects_own_node_ui_manifest(
        "imports/levels/node_ui/tatolab_node_ui.json"
    ));
}
