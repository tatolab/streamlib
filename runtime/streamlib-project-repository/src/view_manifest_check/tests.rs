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

/// A view whose `picture` input is still declared by the older `family` field and whose `level`
/// output names its pin type.
const FAMILY_MANIFEST: &str = r#"{"contract": "1.1", "title": "Monitor", "entry": "index.html", "placement": "node",
  "inputs": [{"name": "picture", "family": "image"}],
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
fn a_port_declared_by_family_fails_the_check_with_the_readers_words_naming_the_pin_type() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "monitor", FAMILY_MANIFEST);

    let checked = check_a_projects_view_manifests(&project_directory, None).unwrap();

    assert_eq!(checked.run.check, ProjectCodeCheckKind::ExtensionManifest);
    assert_eq!(checked.run.state, ProjectCodeCheckState::Failed);
    assert_eq!(
        the_codes_ports_and_severities(&checked),
        [(
            "view_manifest_refused".to_owned(),
            None,
            ProjectCodeCheckSeverity::Error
        )]
    );
    let refused = &checked.findings[0];
    assert_eq!(refused.path, "views/monitor/tatolab_view.json");
    assert_eq!(refused.node_type.as_deref(), Some("acme:monitor"));
    assert!(
        refused.words.starts_with("the view does not open: `inputs[0]`: port `picture` is declared by the older `family` field")
            && refused.words.contains(r#"write `"pin_type": {"type": "texture"}` in place of `"family": "image"`, with `"contract": "1.1"`"#),
        "{}",
        refused.words
    );
    assert_eq!(checked.files_checked.len(), 1);
    assert_eq!(
        checked.files_checked[0].content_sha256,
        content_sha256_of(FAMILY_MANIFEST.as_bytes())
    );
}

#[test]
fn a_manifest_whose_ports_name_pin_types_passes_with_no_finding() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "typed", TYPED_MANIFEST);

    let checked = check_a_projects_view_manifests(&project_directory, None).unwrap();

    assert_eq!(checked.run.state, ProjectCodeCheckState::Passed);
    assert_eq!(checked.findings, []);
    assert_eq!(checked.files_checked.len(), 1);
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
    std::fs::create_dir_all(project_directory.join("views/folded/tatolab_view.json")).unwrap();

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
        checked.findings[2].words.contains("cannot be read"),
        "{}",
        checked.findings[2].words
    );
    assert!(
        checked.findings[3].words.contains("not JSON"),
        "{}",
        checked.findings[3].words
    );
    assert_eq!(
        checked
            .files_checked
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [
            "views/broken/tatolab_view.json",
            "views/garbled/tatolab_view.json"
        ],
        "a manifest whose bytes were not read is no file checked"
    );
}

#[test]
fn only_the_projects_own_views_or_those_among_the_paths_are_checked() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    let mut project_file = a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "monitor", FAMILY_MANIFEST);
    a_view_folder_holding(&project_directory, "typed", TYPED_MANIFEST);
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "levels");
    a_view_folder_holding(&import_directory, "meter", FAMILY_MANIFEST);
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
    assert_eq!(every_own_view.findings.len(), 1);

    let only_typed = check_a_projects_view_manifests(
        &project_directory,
        Some(&["views/typed/tatolab_view.json".to_owned()]),
    )
    .unwrap();
    assert_eq!(only_typed.findings, []);
    assert_eq!(only_typed.files_checked.len(), 1);
}

#[test]
fn an_imports_views_folder_that_does_not_read_keeps_no_own_view_from_being_checked() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme");
    let mut project_file = a_project_named(&project_directory, "acme");
    a_view_folder_holding(&project_directory, "typed", TYPED_MANIFEST);
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "levels");
    std::fs::write(
        import_directory.join(PROJECT_VIEWS_DIRECTORY),
        "not a folder",
    )
    .unwrap();
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
    assert!(
        crate::project_views::every_view_a_project_and_its_imports_bring(&project_directory)
            .is_err(),
        "the import's `views` file is one the import-aware listing refuses"
    );

    let checked = check_a_projects_view_manifests(&project_directory, None).unwrap();

    assert_eq!(checked.files_checked.len(), 1);
}

/// The in-tree views whose ports are still declared by the older `family` field, each because
/// a port carries a record of named numbers, which no pin type a view's port names yet carries.
/// Each leaves this list once its ports name pin types; a view on it that opens fails the test.
const IN_TREE_VIEWS_STILL_DECLARED_BY_FAMILY: [&str; 2] = [
    "examples/gulf-stream-neon/views/stage",
    "examples/paint-pulse/views/stage",
];

/// Every folder of the tree that holds views of its own: each of `packages/` and `examples/`.
fn every_in_tree_folder_that_may_hold_views(workspace_root: &Path) -> Vec<std::path::PathBuf> {
    let mut folders: Vec<_> = ["packages", "examples"]
        .iter()
        .flat_map(|parent| std::fs::read_dir(workspace_root.join(parent)).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|folder| folder.join(PROJECT_VIEWS_DIRECTORY).is_dir())
        .collect();
    folders.sort();
    folders
}

#[test]
fn every_in_tree_view_opens_and_the_check_reports_nothing_for_it() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut views_read = 0;
    let mut not_as_expected = Vec::new();
    for folder in every_in_tree_folder_that_may_hold_views(&workspace_root) {
        let relative_folder = folder
            .strip_prefix(&workspace_root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for view in std::fs::read_dir(folder.join(PROJECT_VIEWS_DIRECTORY)).unwrap() {
            let view = view.unwrap().path();
            let view_path = format!(
                "{relative_folder}/{PROJECT_VIEWS_DIRECTORY}/{}",
                view.file_name().unwrap().to_string_lossy()
            );
            let manifest_json: Value = serde_json::from_slice(
                &std::fs::read(view.join(TATOLAB_VIEW_MANIFEST_FILE_NAME)).unwrap(),
            )
            .unwrap();
            views_read += 1;
            let opens = read_tatolab_view_manifest(&manifest_json);
            match (
                opens,
                IN_TREE_VIEWS_STILL_DECLARED_BY_FAMILY.contains(&view_path.as_str()),
            ) {
                (Ok(_), false) => {}
                (Err(why), true) if why.contains("declared by the older `family` field") => {}
                (Err(why), _) => not_as_expected.push(format!("{view_path} does not open: {why}")),
                (Ok(_), true) => not_as_expected.push(format!(
                    "{view_path} opens: take it off IN_TREE_VIEWS_STILL_DECLARED_BY_FAMILY"
                )),
            }
        }
        if folder.join(PROJECT_FILE_NAME).is_file() {
            let checked = check_a_projects_view_manifests(&folder, None).unwrap();
            not_as_expected.extend(
                checked
                    .findings
                    .iter()
                    .map(|finding| format!("{relative_folder}/{}", finding.path))
                    .filter(|path| {
                        !IN_TREE_VIEWS_STILL_DECLARED_BY_FAMILY
                            .iter()
                            .any(|view| path.starts_with(&format!("{view}/")))
                    })
                    .map(|path| format!("the check reports {path}")),
            );
        }
    }
    assert!(
        views_read > IN_TREE_VIEWS_STILL_DECLARED_BY_FAMILY.len(),
        "the tree's views are found"
    );
    assert!(not_as_expected.is_empty(), "{not_as_expected:#?}");
}
