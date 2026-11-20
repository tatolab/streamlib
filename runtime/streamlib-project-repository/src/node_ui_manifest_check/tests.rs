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
    assert!(to_check.is_judged_against_what_the_projects_modules_declare());
    assert_eq!(
        to_check.file_checked,
        Some(ProjectFileChecked {
            path: "node_ui/tatolab_node_ui.json".to_owned(),
            content_sha256: content_sha256_of(MANIFEST.as_bytes()),
        })
    );

    let declared = WhatTheProjectsModulesDeclare {
        own_node_types: BTreeSet::from(["crossfade:Crossfade".to_owned()]),
        ..WhatTheProjectsModulesDeclare::default()
    };
    let findings = the_findings_of_a_node_ui_manifest(&to_check, Some(&declared));

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

const NAMED_TYPE_DEFAULTS: &str = r#"{"contract": "1.1",
  "controls": [
    {"type": "nodes.sensor:SensorReading", "script": "sensor_reading.ts"},
    {"type": "rig.arm:Pose", "script": "pose.ts"},
    {"type": "nowhere:Thing", "script": "thing.ts"},
    {"type": "rig.arm:Pose", "control": "dial", "script": "pose_dial.ts"}
  ]}"#;

fn what_a_project_declaring_a_sensor_reading_and_importing_a_pose_declares()
-> WhatTheProjectsModulesDeclare {
    WhatTheProjectsModulesDeclare {
        own_node_types: BTreeSet::new(),
        module_declaring_each_named_pin_type: BTreeMap::from([
            (
                "nodes.sensor:SensorReading".to_owned(),
                ModuleThatDeclares::TheProjectsOwnCode,
            ),
            (
                "rig.arm:Pose".to_owned(),
                ModuleThatDeclares::AnotherModule("the import `rig` (`imports/rig`)".to_owned()),
            ),
        ]),
        unreadable_modules: Vec::new(),
    }
}

/// Mental-revert: judging every default control as a built-in type's refuses the project's own
/// `SensorReading` and names no declaring module for `Pose`.
#[test]
fn a_named_types_default_control_passes_only_in_the_module_that_declares_the_type() {
    let project_directory = a_project_holding_the_node_ui_manifest(NAMED_TYPE_DEFAULTS);
    let to_check = the_node_ui_manifest_a_check_reads(project_directory.path(), None)
        .expect("the project has a node UI manifest");
    assert!(to_check.is_judged_against_what_the_projects_modules_declare());

    let findings = the_findings_of_a_node_ui_manifest(
        &to_check,
        Some(&what_a_project_declaring_a_sensor_reading_and_importing_a_pose_declares()),
    );

    let codes_and_words = the_codes_and_words(&findings);
    let [
        (imported_code, imported_words),
        (undeclared_code, undeclared_words),
    ] = codes_and_words.as_slice()
    else {
        panic!("two findings: {findings:?}");
    };
    assert_eq!(
        *imported_code,
        "default_control_not_the_modules_to_register"
    );
    assert!(
        imported_words.contains("`controls[1]` registers the default control of `rig.arm:Pose`")
            && imported_words.contains("register it in the import `rig` (`imports/rig`)"),
        "{imported_words}"
    );
    assert_eq!(
        *undeclared_code,
        "default_control_not_the_modules_to_register"
    );
    assert!(
        undeclared_words.contains("`controls[2]`")
            && undeclared_words.contains("no module the project brings declares"),
        "{undeclared_words}"
    );
    assert!(
        the_findings_of_a_node_ui_manifest(&to_check, None).is_empty(),
        "a named type's default is not judged without what the modules declare"
    );
}

#[test]
fn what_the_modules_declare_is_read_off_the_node_type_listing() {
    let listing: ListProjectNodeTypesToolResult = serde_json::from_value(serde_json::json!({
        "project_directory": "/projects/cell",
        "project": "4a4602b0-9d26-4d3a-b13c-018fcbea7c53",
        "project_name": "cell",
        "sources": [
            {
                "source": "project",
                "node_types": [{
                    "type": "nodes.ik:InverseKinematics", "kind": "process", "description": "",
                    "category": null, "category_from": null, "inputs": [], "outputs": [],
                    "path": "nodes/ik.py", "line": 4,
                }],
                "graph_generators": [],
                "named_pin_types": [{
                    "named_pin_type": "nodes.sensor:SensorReading", "description": "A reading.",
                    "path": "nodes/sensor.py", "line": 9,
                }],
                "unreadable_modules": [],
            },
            {
                "source": "dependency", "project": "a17ac5c3-71fb-42f2-b8ef-a905b9c1c30f",
                "folder": "rig", "version": "v1", "project_name": "Rig",
                "node_types": [{
                    "type": "rig.arm:Arm", "kind": "process", "description": "",
                    "category": null, "category_from": null, "inputs": [], "outputs": [],
                    "path": "imports/rig/rig/arm.py", "line": 2,
                }],
                "graph_generators": [],
                "named_pin_types": [{
                    "named_pin_type": "rig.arm:Pose", "description": "A pose.",
                    "path": "imports/rig/rig/arm.py", "line": 12,
                }],
                "unreadable_modules": [{
                    "module": "rig.hand", "path": "imports/rig/rig/hand.py", "line": 3,
                    "column": 1, "why": "invalid syntax",
                }],
            },
        ],
    }))
    .expect("a listing");

    assert_eq!(
        WhatTheProjectsModulesDeclare::read_off(&listing),
        WhatTheProjectsModulesDeclare {
            own_node_types: BTreeSet::from(["nodes.ik:InverseKinematics".to_owned()]),
            module_declaring_each_named_pin_type: BTreeMap::from([
                (
                    "nodes.sensor:SensorReading".to_owned(),
                    ModuleThatDeclares::TheProjectsOwnCode,
                ),
                (
                    "rig.arm:Pose".to_owned(),
                    ModuleThatDeclares::AnotherModule(
                        "the import `Rig` (`imports/rig`)".to_owned()
                    ),
                ),
            ]),
            unreadable_modules: vec![ModuleNotRead {
                module: ModuleThatDeclares::AnotherModule(
                    "the import `Rig` (`imports/rig`)".to_owned()
                ),
                path: "imports/rig/rig/hand.py".to_owned(),
            }],
        }
    );
}

/// Mental-revert: judging without the modules that do not read refuses `nowhere:Thing` and
/// `elsewhere:Mixer` as declared nowhere, telling the author to declare what may already be.
#[test]
fn an_entry_a_module_that_does_not_read_may_declare_is_a_warning_naming_that_module() {
    let named = a_project_holding_the_node_ui_manifest(NAMED_TYPE_DEFAULTS);
    let named = the_node_ui_manifest_a_check_reads(named.path(), None).unwrap();
    let bodies = a_project_holding_the_node_ui_manifest(MANIFEST);
    let bodies = the_node_ui_manifest_a_check_reads(bodies.path(), None).unwrap();
    let an_import_does_not_read = WhatTheProjectsModulesDeclare {
        own_node_types: BTreeSet::from(["crossfade:Crossfade".to_owned()]),
        unreadable_modules: vec![ModuleNotRead {
            module: ModuleThatDeclares::AnotherModule(
                "the import `rig` (`imports/rig`)".to_owned(),
            ),
            path: "imports/rig/rig/hand.py".to_owned(),
        }],
        ..what_a_project_declaring_a_sensor_reading_and_importing_a_pose_declares()
    };

    let findings = the_findings_of_a_node_ui_manifest(&named, Some(&an_import_does_not_read));
    let [imported, not_judged] = findings.as_slice() else {
        panic!("two findings: {findings:?}");
    };
    assert_eq!(
        imported.finding_code,
        "default_control_not_the_modules_to_register"
    );
    assert_eq!(not_judged.finding_code, "default_control_not_judged");
    assert_eq!(not_judged.severity, ProjectCodeCheckSeverity::Warning);
    assert!(
        not_judged.words.contains("`controls[2]`")
            && not_judged.words.contains("`nowhere:Thing`")
            && not_judged
                .words
                .contains("`imports/rig/rig/hand.py` does not parse"),
        "{}",
        not_judged.words
    );

    let body_findings = the_findings_of_a_node_ui_manifest(&bodies, Some(&an_import_does_not_read));
    assert_eq!(
        the_codes_and_words(&body_findings)
            .into_iter()
            .map(|(code, _)| code)
            .collect::<Vec<_>>(),
        [
            "default_control_not_the_modules_to_register",
            "node_body_not_the_modules_to_register"
        ],
        "a node body is the project's own code's, which an import that does not read cannot declare"
    );

    let own_code_does_not_read = WhatTheProjectsModulesDeclare {
        unreadable_modules: vec![ModuleNotRead {
            module: ModuleThatDeclares::TheProjectsOwnCode,
            path: "nodes/mixer.py".to_owned(),
        }],
        ..an_import_does_not_read
    };
    let body_findings = the_findings_of_a_node_ui_manifest(&bodies, Some(&own_code_does_not_read));
    let [_, body] = body_findings.as_slice() else {
        panic!("two findings: {body_findings:?}");
    };
    assert_eq!(body.finding_code, "node_body_not_judged");
    assert_eq!(body.severity, ProjectCodeCheckSeverity::Warning);
    assert!(
        body.words.contains("`elsewhere:Mixer`") && body.words.contains("`nodes/mixer.py`"),
        "{}",
        body.words
    );
}

#[test]
fn a_manifest_that_does_not_read_is_an_error_and_one_outside_the_paths_is_not_read() {
    let project_directory = a_project_holding_the_node_ui_manifest(r#"{"contract": "2.0"}"#);
    let to_check = the_node_ui_manifest_a_check_reads(project_directory.path(), None)
        .expect("the project has a node UI manifest");
    assert!(!to_check.is_judged_against_what_the_projects_modules_declare());

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
}
