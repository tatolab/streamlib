// Copyright (c) 2025 Tatolab LLC
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

const DECK_MANIFEST: &str = r#"{"contract": "1.1", "title": "Deck", "entry": "index.html", "placement": "node", "outputs": [{"name": "fader", "pin_type": {"type": "tensor"}}]}"#;

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
        [
            format!("{}:deck", project_file.project),
            format!("{}:meter", imported_project.project)
        ]
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
    let acme_dj = a_project_named(workspace.path(), "acme-dj").project;
    std::fs::create_dir_all(workspace.path().join("views/empty")).unwrap();
    a_view_folder_holding(workspace.path(), "garbled", "{ not json");
    a_view_folder_holding(
        workspace.path(),
        "late",
        r#"{"contract": "1.2", "title": "Late", "entry": "index.html", "placement": "area"}"#,
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

    assert_eq!(refusals[0].0, the_view_type_of(acme_dj, "empty"));
    assert_eq!(
        refusals[0].1,
        "the view's folder has no `tatolab_view.json`"
    );
    assert_eq!(refusals[1].0, the_view_type_of(acme_dj, "garbled"));
    assert!(
        refusals[1]
            .1
            .starts_with("`tatolab_view.json` is not JSON: "),
        "{}",
        refusals[1].1
    );
    assert_eq!(views[1].manifest_as_written, None);
    assert_eq!(refusals[2].0, the_view_type_of(acme_dj, "late"));
    assert_eq!(
        refusals[2].1,
        "`contract`: this runtime reads view contract 1.1."
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
    let acme_dj = a_project_named(&project_directory, "acme-dj");
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
        [
            format!("{}:deck", acme_dj.project),
            format!("{}:meter", core_project.project)
        ]
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

/// A graph document of one stream holding a node of each type in `node_types`.
fn a_stream_placing(node_types: &[&str]) -> serde_json::Value {
    json!({
        "asset": Uuid::new_v4(),
        "nodes": node_types
            .iter()
            .enumerate()
            .map(|(index, node_type)| json!({
                "id": Uuid::new_v4(),
                "name": format!("node_{index}"),
                "type": node_type,
            }))
            .collect::<Vec<_>>(),
        "links": [],
    })
}

fn the_node_types_of(graph_document: &serde_json::Value) -> Vec<String> {
    graph_document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["type"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_stream_placing_a_view_by_its_projects_name_reads_as_placing_it_by_its_projects_uuid() {
    let workspace = tempfile::tempdir().unwrap();
    let project_directory = workspace.path().join("acme-dj");
    let mut project_file = a_project_named(&project_directory, "Acme DJ v1.2");
    a_view_folder_holding(&project_directory, "deck", DECK_MANIFEST);
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "Mr. Levels");
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
    let deck = the_view_type_of(project_file.project, "deck");
    let meter = the_view_type_of(imported_project.project, "meter");

    let read = crate::graph_document_as_its_project_reads_it(
        &project_directory,
        &a_stream_placing(&[
            "Acme DJ v1.2:deck",
            "Mr. Levels:meter",
            &deck,
            "Acme DJ v1.2:mixer",
            "tatolab.testpattern:TestPatternSource",
        ]),
    )
    .unwrap();

    assert_eq!(
        the_node_types_of(&read),
        [
            deck.as_str(),
            meter.as_str(),
            deck.as_str(),
            "Acme DJ v1.2:mixer",
            "tatolab.testpattern:TestPatternSource"
        ]
    );
}

#[test]
fn a_copy_of_a_project_places_its_own_views_by_the_copys_uuid_in_each_of_its_streams() {
    let workspace = tempfile::tempdir().unwrap();
    let samples_directory = workspace.path().join("samples");
    let sample_directory = samples_directory.join("acme-dj");
    let sample = a_project_named(&sample_directory, "Acme DJ");
    a_view_folder_holding(&sample_directory, "deck", DECK_MANIFEST);
    std::fs::create_dir_all(sample_directory.join("streams")).unwrap();
    let deck_in_the_sample = the_view_type_of(sample.project, "deck");
    for (stream, node_types) in [
        (
            "main",
            vec![
                deck_in_the_sample.as_str(),
                "tatolab.testpattern:TestPatternSource",
            ],
        ),
        ("legacy", vec!["Acme DJ:deck"]),
        ("plain", vec!["tatolab.testpattern:TestPatternSource"]),
    ] {
        std::fs::write(
            sample_directory.join(format!("streams/{stream}.graph.json")),
            serde_json::to_vec_pretty(&a_stream_placing(&node_types)).unwrap(),
        )
        .unwrap();
    }
    let plain_before = std::fs::read(sample_directory.join("streams/plain.graph.json")).unwrap();
    let copy_directory = workspace.path().join("my-set");
    let copy = Uuid::new_v4();

    crate::the_sample_project_named(Some(&samples_directory), "acme-dj")
        .unwrap()
        .copy_into(&copy_directory, copy, "my-set")
        .unwrap();

    let node_types_of_the_copys = |stream: &str| {
        the_node_types_of(
            &serde_json::from_slice(
                &std::fs::read(copy_directory.join(format!("streams/{stream}.graph.json")))
                    .unwrap(),
            )
            .unwrap(),
        )
    };
    let deck_in_the_copy = the_view_type_of(copy, "deck");
    assert_eq!(
        node_types_of_the_copys("main"),
        [
            deck_in_the_copy.as_str(),
            "tatolab.testpattern:TestPatternSource"
        ]
    );
    assert_eq!(
        node_types_of_the_copys("legacy"),
        [deck_in_the_copy.as_str()]
    );
    assert_eq!(
        std::fs::read(copy_directory.join("streams/plain.graph.json")).unwrap(),
        plain_before,
        "a stream placing none of the views is copied as it was"
    );
}
