// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use streamlib_node_schema::pin_type::BuiltInPinTypeName;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeUiControlPinType;

use super::*;
use crate::project_file::{PROJECT_FILE_NAME, ProjectImport};

fn a_project_named(project_directory: &Path, name: &str) -> TatolabProjectFile {
    let project_file = TatolabProjectFile::of_a_new_project(name);
    std::fs::create_dir_all(project_directory).unwrap();
    std::fs::write(
        project_directory.join(PROJECT_FILE_NAME),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();
    project_file
}

fn a_node_ui_manifest_in(module_directory: &Path, manifest: &str) {
    let folder = module_directory.join(NODE_UI_DIRECTORY);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(NODE_UI_MANIFEST_FILE_NAME), manifest).unwrap();
}

/// The project `acme` with its own manifest, importing `levels`, which registers a control,
/// and `broken`, whose manifest names a later minor.
fn a_project_with_two_imports(workspace: &Path) -> std::path::PathBuf {
    let project_directory = workspace.join("acme");
    let mut project_file = a_project_named(&project_directory, "acme");
    a_node_ui_manifest_in(
        &project_directory,
        r#"{"contract": "1.1",
            "controls": [{"type": "float", "control": "knob", "script": "knob.ts"}],
            "node_bodies": [{"node_type": "crossfade:Crossfade", "script": "bodies/crossfade.ts"}]}"#,
    );
    for (folder, manifest) in [
        (
            "levels",
            r#"{"contract": "1.1", "controls": [{"type": "audio", "control": "meter", "script": "meter.ts"}]}"#,
        ),
        ("broken", r#"{"contract": "1.2"}"#),
    ] {
        let import_directory = project_directory
            .join(PROJECT_IMPORTS_DIRECTORY)
            .join(folder);
        let imported_project = a_project_named(&import_directory, folder);
        a_node_ui_manifest_in(&import_directory, manifest);
        project_file.dependencies.push(ProjectImport {
            project: imported_project.project,
            folder: folder.into(),
            source: format!("/elsewhere/{folder}"),
            version: "0".repeat(64),
            copied_files: BTreeMap::new(),
        });
    }
    std::fs::write(
        project_directory.join(PROJECT_FILE_NAME),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();
    project_directory
}

#[test]
fn each_element_is_listed_with_its_module_its_file_and_what_it_is_for_and_a_manifest_that_does_not_read_is_a_finding()
 {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = a_project_with_two_imports(workspace.path());

    let listed = every_node_ui_element_a_project_and_its_imports_bring(&project_directory).unwrap();

    let rows: Vec<(Option<&str>, &str, &str, &ProjectNodeUiElement)> = listed
        .elements
        .iter()
        .map(|row| {
            (
                row.module.import_folder.as_deref(),
                row.module.project_name.as_str(),
                row.script_path.as_str(),
                &row.element,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                None,
                "acme",
                "node_ui/knob.ts",
                &ProjectNodeUiElement::Control {
                    pin_type: NodeUiControlPinType::BuiltIn(BuiltInPinTypeName::Float),
                    control: Some("knob".to_owned()),
                }
            ),
            (
                None,
                "acme",
                "node_ui/bodies/crossfade.ts",
                &ProjectNodeUiElement::NodeBody {
                    node_type: "crossfade:Crossfade".to_owned(),
                }
            ),
            (
                Some("levels"),
                "levels",
                "imports/levels/node_ui/meter.ts",
                &ProjectNodeUiElement::Control {
                    pin_type: NodeUiControlPinType::BuiltIn(BuiltInPinTypeName::Audio),
                    control: Some("meter".to_owned()),
                }
            ),
        ]
    );
    assert_eq!(
        listed
            .manifests
            .iter()
            .map(|listed| listed.manifest_path.as_str())
            .collect::<Vec<_>>(),
        [
            "node_ui/tatolab_node_ui.json",
            "imports/levels/node_ui/tatolab_node_ui.json"
        ]
    );
    assert_eq!(listed.manifests[0].manifest.node_bodies.len(), 1);
    let [not_read] = listed.unreadable_manifests.as_slice() else {
        panic!("one manifest does not read: {listed:?}");
    };
    assert_eq!(
        not_read.manifest_path,
        "imports/broken/node_ui/tatolab_node_ui.json"
    );
    assert_eq!(not_read.module.import_folder.as_deref(), Some("broken"));
    assert!(not_read.why.contains("`contract`"), "{}", not_read.why);
}

#[test]
fn a_module_with_no_manifest_registers_nothing_and_one_that_is_not_json_says_so() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    a_project_named(&project_directory, "acme");

    let listed = every_node_ui_element_a_project_and_its_imports_bring(&project_directory).unwrap();
    assert_eq!(listed.elements, []);
    assert_eq!(listed.manifests, []);
    assert_eq!(listed.unreadable_manifests, []);

    a_node_ui_manifest_in(&project_directory, "{not json");
    let listed = every_node_ui_element_a_project_and_its_imports_bring(&project_directory).unwrap();
    let [not_read] = listed.unreadable_manifests.as_slice() else {
        panic!("the manifest does not read: {listed:?}");
    };
    assert_eq!(not_read.manifest_path, "node_ui/tatolab_node_ui.json");
    assert!(not_read.why.contains("is not JSON"), "{}", not_read.why);
}
