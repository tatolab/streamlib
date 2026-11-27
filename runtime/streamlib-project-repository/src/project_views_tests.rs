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
        &crate::StreamDocumentsAnEditStaged::default(),
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
    a_copy_of_acme_dj_places_its_deck_in_each_of_its_streams_as(DECK_MANIFEST, |copy| {
        the_view_type_of(copy, "deck")
    });
}

/// Mental-revert: retyping to the deck's folder in the copy leaves the copy's streams placing
/// `<copy uuid>:deck` where the deck names a UUID of its own.
#[test]
fn a_copy_of_a_project_places_its_own_views_naming_a_uuid_by_that_uuid_which_it_keeps() {
    a_copy_of_acme_dj_places_its_deck_in_each_of_its_streams_as(
        &the_deck_manifest_naming(DECK_VIEW),
        |_| DECK_VIEW.to_owned(),
    );
}

/// Copy the sample `Acme DJ`, its view `deck` of `deck_manifest` and streams placing it by its
/// folder in the sample's UUID and in its name, and check each of the copy's streams places it
/// as `deck_in_the_copy` answers for the copy's UUID, its manifest kept as it was.
fn a_copy_of_acme_dj_places_its_deck_in_each_of_its_streams_as(
    deck_manifest: &str,
    deck_in_the_copy: impl Fn(Uuid) -> String,
) {
    let workspace = tempfile::tempdir().unwrap();
    let samples_directory = workspace.path().join("samples");
    let sample_directory = samples_directory.join("acme-dj");
    let sample = a_project_named(&sample_directory, "Acme DJ");
    a_view_folder_holding(&sample_directory, "deck", deck_manifest);
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
    std::fs::create_dir_all(copy_directory.join("streams")).unwrap();
    let already_there = serde_json::to_vec_pretty(&a_stream_placing(&["Acme DJ:deck"])).unwrap();
    std::fs::write(
        copy_directory.join("streams/already-there.graph.json"),
        &already_there,
    )
    .unwrap();
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
    let deck_in_the_copy = deck_in_the_copy(copy);
    assert_eq!(
        std::fs::read_to_string(copy_directory.join("views/deck/tatolab_view.json")).unwrap(),
        deck_manifest
    );
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
    assert_eq!(
        std::fs::read(copy_directory.join("streams/already-there.graph.json")).unwrap(),
        already_there,
        "a stream the copy did not write is left as it was"
    );
}

const DECK_VIEW: &str = "3b8e6f21-9c47-4d05-a1e3-7f2d9b6c4a18";

fn the_deck_manifest_naming(view: &str) -> String {
    format!(
        r#"{{"contract": "1.1", "view": "{view}", "title": "Deck", "entry": "index.html", "placement": "node"}}"#
    )
}

/// A project `Acme DJ` at `acme-dj/` in `workspace` with the view `deck` naming `deck_view`, and
/// its import `levels`, the project `Mr. Levels`, with the view `meter`, whose manifest is
/// `meter_manifest`; the project's file and the import's.
fn acme_dj_importing_levels(
    workspace: &Path,
    deck_view: &str,
    meter_manifest: &str,
) -> (PathBuf, TatolabProjectFile, TatolabProjectFile) {
    let project_directory = workspace.join("acme-dj");
    let mut project_file = a_project_named(&project_directory, "Acme DJ");
    a_view_folder_holding(
        &project_directory,
        "deck",
        &the_deck_manifest_naming(deck_view),
    );
    let import_directory = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels");
    let imported_project = a_project_named(&import_directory, "Mr. Levels");
    a_view_folder_holding(&import_directory, "meter", meter_manifest);
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
    (project_directory, project_file, imported_project)
}

/// Mental-revert: typing a view by its folder lists `deck` as `<project uuid>:deck`.
#[test]
fn a_view_naming_its_own_uuid_is_typed_by_it_and_one_naming_none_by_its_folder() {
    let workspace = tempfile::tempdir().unwrap();
    let (project_directory, project_file, imported_project) =
        acme_dj_importing_levels(workspace.path(), DECK_VIEW, METER_MANIFEST);

    let views = every_view_a_project_and_its_imports_bring(&project_directory).unwrap();

    assert_eq!(
        views
            .iter()
            .map(|view| (view.view_type.clone(), view.view))
            .collect::<Vec<_>>(),
        [
            (DECK_VIEW.to_owned(), Some(DECK_VIEW.parse().unwrap())),
            (the_view_type_of(imported_project.project, "meter"), None),
        ]
    );
    assert!(views[0].manifest.is_ok(), "{:?}", views[0].manifest);
    assert_eq!(
        views[0].manifest.as_ref().unwrap().view,
        Some(DECK_VIEW.parse().unwrap())
    );
    assert_ne!(
        views[0].view_type,
        the_view_type_of(project_file.project, "deck")
    );
}

/// Mental-revert: listing a view whose UUID another holds as it is lists two views of one type.
#[test]
fn a_view_naming_the_uuid_an_earlier_view_names_does_not_open_naming_both_folders() {
    let workspace = tempfile::tempdir().unwrap();
    let (project_directory, project_file, imported_project) = acme_dj_importing_levels(
        workspace.path(),
        DECK_VIEW,
        &the_deck_manifest_naming(DECK_VIEW),
    );
    a_view_folder_holding(
        &project_directory,
        "mixer",
        &the_deck_manifest_naming(DECK_VIEW),
    );

    let views = every_view_a_project_and_its_imports_bring(&project_directory).unwrap();

    assert_eq!(
        views
            .iter()
            .map(|view| view.view_type.clone())
            .collect::<Vec<_>>(),
        [
            DECK_VIEW.to_owned(),
            the_view_type_of(project_file.project, "mixer"),
            the_view_type_of(imported_project.project, "meter"),
        ]
    );
    assert!(views[0].manifest.is_ok());
    for (view, folder) in [
        (&views[1], "views/mixer"),
        (&views[2], "imports/levels/views/meter"),
    ] {
        let refusal = view.manifest.as_ref().unwrap_err();
        assert!(
            refusal.contains("`views/deck`") && refusal.contains(&format!("`{folder}`")),
            "{refusal}"
        );
        assert_eq!(view.view, None);
    }
}

/// Mental-revert: reading the stream as written leaves each node typed by a folder.
#[test]
fn a_stream_placing_a_view_by_its_folder_reads_as_placing_it_by_the_views_uuid() {
    let workspace = tempfile::tempdir().unwrap();
    let meter_view = "8d2f5a90-6c14-4e7b-b3a8-1e9c7d5f0b42";
    let (project_directory, project_file, imported_project) = acme_dj_importing_levels(
        workspace.path(),
        DECK_VIEW,
        &format!(
            r#"{{"contract": "1.0", "view": "{meter_view}", "title": "Meter", "entry": "index.html", "placement": "area"}}"#
        ),
    );
    let deck_by_its_folder = the_view_type_of(project_file.project, "deck");
    let meter = the_view_type_of(imported_project.project, "meter");

    let read = crate::graph_document_as_its_project_reads_it(
        &project_directory,
        &a_stream_placing(&[
            "Acme DJ:deck",
            &deck_by_its_folder,
            DECK_VIEW,
            "Mr. Levels:meter",
            &meter,
            "tatolab.testpattern:TestPatternSource",
        ]),
        &crate::StreamDocumentsAnEditStaged::default(),
    )
    .unwrap();

    assert_eq!(
        the_node_types_of(&read),
        [
            DECK_VIEW,
            DECK_VIEW,
            DECK_VIEW,
            meter_view,
            meter_view,
            "tatolab.testpattern:TestPatternSource"
        ]
    );
}

/// Mental-revert: listing every project's views before keeping the placed ones fails on the
/// import's `views/`, and the deck stays placed by its project's name.
#[test]
fn a_stream_placing_a_view_by_its_folder_reads_when_an_import_it_does_not_place_has_no_views_read()
{
    let workspace = tempfile::tempdir().unwrap();
    let (project_directory, _, _) =
        acme_dj_importing_levels(workspace.path(), DECK_VIEW, METER_MANIFEST);
    let import_views = project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join("levels")
        .join(PROJECT_VIEWS_DIRECTORY);
    std::fs::remove_dir_all(&import_views).unwrap();
    std::fs::write(&import_views, "not a folder").unwrap();

    let read = crate::graph_document_as_its_project_reads_it(
        &project_directory,
        &a_stream_placing(&["Acme DJ:deck"]),
        &crate::StreamDocumentsAnEditStaged::default(),
    )
    .unwrap();

    assert_eq!(the_node_types_of(&read), [DECK_VIEW]);
}

/// Mental-revert: leaving a manifest naming no `view` as it is types its view by its folder.
#[cfg(feature = "history")]
#[test]
fn each_own_view_naming_no_uuid_is_given_one_beside_its_contract_once() {
    let workspace = tempfile::tempdir().unwrap();
    let (project_directory, _, _) =
        acme_dj_importing_levels(workspace.path(), DECK_VIEW, METER_MANIFEST);
    a_view_folder_holding(&project_directory, "meter", METER_MANIFEST);
    a_view_folder_holding(&project_directory, "garbled", "{not json");
    let deck_as_written =
        std::fs::read(project_directory.join("views/deck/tatolab_view.json")).unwrap();
    let the_imports_meter = project_directory.join("imports/levels/views/meter/tatolab_view.json");

    give_each_own_view_naming_no_uuid_one(&project_directory);
    let meter_given: Value = serde_json::from_slice(
        &std::fs::read(project_directory.join("views/meter/tatolab_view.json")).unwrap(),
    )
    .unwrap();
    give_each_own_view_naming_no_uuid_one(&project_directory);

    let view = meter_given["view"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    assert_eq!(
        meter_given
            .as_object()
            .unwrap()
            .keys()
            .take(2)
            .collect::<Vec<_>>(),
        ["contract", "view"]
    );
    let meter_now: Value = serde_json::from_slice(
        &std::fs::read(project_directory.join("views/meter/tatolab_view.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meter_now, meter_given, "a view given a UUID keeps it");
    assert_eq!(
        std::fs::read(project_directory.join("views/deck/tatolab_view.json")).unwrap(),
        deck_as_written
    );
    assert_eq!(
        std::fs::read(project_directory.join("views/garbled/tatolab_view.json")).unwrap(),
        b"{not json"
    );
    assert_eq!(
        std::fs::read_to_string(&the_imports_meter).unwrap(),
        METER_MANIFEST
    );
    let views = every_view_a_project_brings_itself(&project_directory).unwrap();
    let meter = views
        .iter()
        .find(|listed| listed.view_folder_name == "meter")
        .unwrap();
    assert_eq!(meter.view_type, view.to_string());
    assert!(meter.manifest.is_ok(), "{:?}", meter.manifest);
}
