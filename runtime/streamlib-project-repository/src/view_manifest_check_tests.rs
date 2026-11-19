// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use super::*;
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::{PROJECT_FILE_NAME, ProjectImport, TatolabProjectFile};

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

fn a_view_folder_holding(project_directory: &Path, view_folder_name: &str, manifest: &str) {
    let folder = project_directory
        .join(PROJECT_VIEWS_DIRECTORY)
        .join(view_folder_name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME), manifest).unwrap();
}

/// A view whose `picture` input is declared by `family` alone and whose `level` output names
/// its pin type.
const MIXED_MANIFEST: &str = r#"{"contract": "1.1", "title": "Monitor", "entry": "index.html", "placement": "node",
  "inputs": [{"name": "picture", "family": "image"}, {"name": "gain", "family": "channels"}],
  "outputs": [{"name": "level", "pin_type": {"type": "float"}}]}"#;

const TYPED_MANIFEST: &str = r#"{"contract": "1.1", "title": "Typed", "entry": "index.html", "placement": "node",
  "inputs": [{"name": "picture", "pin_type": {"type": "texture"}}]}"#;

fn the_codes_ports_and_severities(
    checked: &ViewManifestsChecked,
) -> Vec<(String, Option<String>, ProjectCodeCheckSeverity)> {
    checked
        .findings
        .iter()
        .map(|finding| {
            (
                finding.finding_code.clone(),
                finding.port.clone(),
                finding.severity,
            )
        })
        .collect()
}

#[test]
fn a_port_declared_by_family_is_a_warning_naming_the_pin_type_to_write_and_the_check_passes() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "monitor", MIXED_MANIFEST);

    let checked = check_a_projects_view_manifests(&project_directory, None).unwrap();

    assert_eq!(checked.run.check, ProjectCodeCheckKind::ExtensionManifest);
    assert_eq!(checked.run.state, ProjectCodeCheckState::Passed);
    assert_eq!(
        the_codes_ports_and_severities(&checked),
        [
            (
                "port_declared_by_family".to_owned(),
                Some("picture".to_owned()),
                ProjectCodeCheckSeverity::Warning
            ),
            (
                "port_declared_by_family".to_owned(),
                Some("gain".to_owned()),
                ProjectCodeCheckSeverity::Warning
            ),
        ]
    );
    let picture = &checked.findings[0];
    assert_eq!(picture.path, "views/monitor/tatolab_view.json");
    assert_eq!(picture.node_type.as_deref(), Some("acme:monitor"));
    assert!(
        picture.words.contains(r#"write `"pin_type": {"type": "texture"}` in place of `"family": "image"`, with `"contract": "1.1"`"#),
        "{}",
        picture.words
    );
    let gain = &checked.findings[1].words;
    assert!(
        gain.contains("a `channels` port carries one of `boolean`, `integer`, `float`, `color`, `vector`, `rectangle`, `transform`, `audio`"),
        "{gain}"
    );
    assert!(
        !gain.contains("write `\"pin_type\": {\"type\": \"float\"}`"),
        "a guessed type: {gain}"
    );
    assert_eq!(checked.files_checked.len(), 1);
    assert_eq!(
        checked.files_checked[0].path,
        "views/monitor/tatolab_view.json"
    );
    assert_eq!(
        checked.files_checked[0].content_sha256,
        content_sha256_of(MIXED_MANIFEST.as_bytes())
    );
}

#[test]
fn a_manifest_that_does_not_open_fails_the_check_with_the_readers_words() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    a_project_named(&project_directory, "acme");
    a_view_folder_holding(
        &project_directory,
        "broken",
        r#"{"contract": "1.1", "title": "Broken", "entry": "index.html", "placement": "node",
            "inputs": [{"name": "picture", "pin_type": {"type": "picture"}}]}"#,
    );
    a_view_folder_holding(&project_directory, "garbled", "{not json");
    std::fs::create_dir_all(project_directory.join("views/empty")).unwrap();

    let checked = check_a_projects_view_manifests(&project_directory, None).unwrap();

    assert_eq!(checked.run.state, ProjectCodeCheckState::Failed);
    assert_eq!(
        the_codes_ports_and_severities(&checked),
        [
            (
                "view_manifest_refused".to_owned(),
                None,
                ProjectCodeCheckSeverity::Error
            ),
            (
                "view_manifest_missing".to_owned(),
                None,
                ProjectCodeCheckSeverity::Error
            ),
            (
                "view_manifest_refused".to_owned(),
                None,
                ProjectCodeCheckSeverity::Error
            ),
        ]
    );
    assert!(
        checked.findings[0].words.contains("`picture`"),
        "{}",
        checked.findings[0].words
    );
    assert!(
        checked.findings[2].words.contains("not JSON"),
        "{}",
        checked.findings[2].words
    );
}

#[test]
fn only_the_projects_own_views_or_those_among_the_paths_are_checked() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    let mut project_file = a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "monitor", MIXED_MANIFEST);
    a_view_folder_holding(&project_directory, "typed", TYPED_MANIFEST);
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "levels");
    a_view_folder_holding(&import_directory, "meter", MIXED_MANIFEST);
    project_file.dependencies.push(ProjectImport {
        project: imported_project.project,
        folder: "levels".into(),
        source: "/elsewhere/levels".into(),
        version: "0".repeat(64),
        copied_files: BTreeMap::new(),
    });
    std::fs::write(
        project_directory.join(PROJECT_FILE_NAME),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();

    let every_own_view = check_a_projects_view_manifests(&project_directory, None).unwrap();
    assert_eq!(
        every_own_view
            .files_checked
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [
            "views/monitor/tatolab_view.json",
            "views/typed/tatolab_view.json"
        ]
    );
    assert_eq!(every_own_view.findings.len(), 2);

    let only_typed = check_a_projects_view_manifests(
        &project_directory,
        Some(&["views/typed/tatolab_view.json".to_owned()]),
    )
    .unwrap();
    assert_eq!(only_typed.findings, []);
    assert_eq!(only_typed.files_checked.len(), 1);
}
