// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use serde_json::json;

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

fn a_view_folder_holding(project_directory: &Path, view_folder_name: &str, manifest: &str) {
    let folder = project_directory
        .join(PROJECT_VIEWS_DIRECTORY)
        .join(view_folder_name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME), manifest).unwrap();
}

const DECK_MANIFEST: &str = r#"{"contract": "1.0", "title": "Deck", "entry": "index.html", "placement": "node", "outputs": [{"name": "fader", "family": "array"}]}"#;

const METER_MANIFEST: &str =
    r#"{"contract": "1.0", "title": "Meter", "entry": "index.html", "placement": "area"}"#;

#[test]
fn a_projects_views_and_its_imports_views_are_listed_each_under_the_project_bringing_it() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme-dj");
    let mut project_file = a_project_named(&project_directory, "acme-dj");
    a_view_folder_holding(&project_directory, "deck", DECK_MANIFEST);
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "levels");
    a_view_folder_holding(&import_directory, "meter", METER_MANIFEST);
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

    let views = every_view_a_project_and_its_imports_bring(&project_directory).unwrap();

    assert_eq!(
        views
            .iter()
            .map(|view| view.view_type.as_str())
            .collect::<Vec<_>>(),
        ["acme-dj:deck", "levels:meter"]
    );
    assert_eq!(
        views[0].brought_by,
        ProjectBringingAView {
            project: project_file.project,
            project_name: "acme-dj".into(),
            import_folder: None,
        }
    );
    assert_eq!(
        views[0].folder,
        project_directory.join(PROJECT_VIEWS_DIRECTORY).join("deck")
    );
    assert_eq!(views[0].manifest.as_ref().unwrap().outputs[0].name, "fader");
    assert_eq!(
        views[1].brought_by,
        ProjectBringingAView {
            project: imported_project.project,
            project_name: "levels".into(),
            import_folder: Some("levels".into()),
        }
    );
    assert_eq!(views[1].manifest.as_ref().unwrap().title, "Meter");
}

#[test]
fn a_view_folder_without_a_readable_manifest_is_listed_with_why_it_does_not_open() {
    let workspace = tempfile::tempdir().unwrap();
    a_project_named(workspace.path(), "acme-dj");
    std::fs::create_dir_all(workspace.path().join("views/empty")).unwrap();
    a_view_folder_holding(workspace.path(), "garbled", "{ not json");
    a_view_folder_holding(
        workspace.path(),
        "late",
        r#"{"contract": "1.1", "title": "Late", "entry": "index.html", "placement": "area"}"#,
    );
    std::fs::write(workspace.path().join("views/notes.txt"), "not a view").unwrap();

    let views = every_view_a_project_and_its_imports_bring(workspace.path()).unwrap();
    let refusals: Vec<(&str, &str)> = views
        .iter()
        .map(|view| {
            (
                view.view_type.as_str(),
                view.manifest.as_ref().unwrap_err().as_str(),
            )
        })
        .collect();

    assert_eq!(refusals[0].0, "acme-dj:empty");
    assert_eq!(
        refusals[0].1,
        "the view's folder has no `tatolab_view.json`"
    );
    assert_eq!(refusals[1].0, "acme-dj:garbled");
    assert!(
        refusals[1]
            .1
            .starts_with("`tatolab_view.json` is not JSON: "),
        "{}",
        refusals[1].1
    );
    assert_eq!(views[1].manifest_as_written, None);
    assert_eq!(
        refusals[2],
        (
            "acme-dj:late",
            "`contract`: this runtime reads view contract 1.0."
        )
    );
    assert_eq!(
        views[2].manifest_as_written.as_ref().unwrap()["title"],
        json!("Late")
    );
    assert_eq!(views.len(), 3);
}

#[test]
fn a_project_with_no_views_folder_lists_none() {
    let workspace = tempfile::tempdir().unwrap();
    a_project_named(workspace.path(), "acme-dj");

    assert_eq!(
        every_view_a_project_and_its_imports_bring(workspace.path()).unwrap(),
        []
    );
}

#[test]
fn a_directory_holding_no_project_is_refused() {
    let workspace = tempfile::tempdir().unwrap();

    assert!(matches!(
        every_view_a_project_and_its_imports_bring(workspace.path()),
        Err(ProjectRepositoryRefusal::NotAProject { .. })
    ));
}

#[test]
fn the_core_projects_views_are_listed_after_the_projects_own_through_its_link() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme-dj");
    a_project_named(&project_directory, "acme-dj");
    a_view_folder_holding(&project_directory, "deck", DECK_MANIFEST);
    let core_project_directory = workspace.path().join("unit/tatolab-core-nodes");
    let core_project = a_project_named(&core_project_directory, "Core Nodes");
    a_view_folder_holding(&core_project_directory, "meter", METER_MANIFEST);
    crate::core_project_import::link_the_core_project_into(
        &project_directory,
        &core_project_directory,
    )
    .unwrap();

    let views = every_view_a_project_and_its_imports_bring(&project_directory).unwrap();

    assert_eq!(
        views
            .iter()
            .map(|view| view.view_type.as_str())
            .collect::<Vec<_>>(),
        ["acme-dj:deck", "Core Nodes:meter"]
    );
    assert_eq!(
        views[1].brought_by,
        ProjectBringingAView {
            project: core_project.project,
            project_name: "Core Nodes".into(),
            import_folder: Some(crate::core_project_import::CORE_PROJECT_IMPORT_FOLDER.into()),
        }
    );
}
