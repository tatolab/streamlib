// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::NodeDescriptor;
use streamlib_project_repository::{PROJECT_VIEWS_DIRECTORY, TATOLAB_VIEW_MANIFEST_FILE_NAME};
use crate::core::runtime::streams_loaded_in_this_runtime::operations_on_the_streams_loaded_in_this_runtime::OperationsOnTheStreamsLoadedInThisRuntime;

/// A project directory holding only a venv, so a create writes every scaffold file.
fn a_directory_holding_only_a_venv() -> ProjectWithGraphDocuments {
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    std::fs::remove_file(project.path().join("pyproject.toml")).unwrap();
    std::fs::remove_file(project.path().join(PROJECT_FILE_NAME)).unwrap();
    project
}

#[test]
#[serial]
fn a_create_project_writes_the_scaffold_commits_it_and_loads_its_stream_under_the_folders_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let first_commits = Arc::new(Mutex::new(Vec::new()));
    let first_commits_made = Arc::clone(&first_commits);
    runner
        .set_created_project_first_commit(Arc::new(move |project_directory: &Path| {
            first_commits_made
                .lock()
                .push(project_directory.to_path_buf());
            Ok(())
        }))
        .unwrap();
    let project = a_directory_holding_only_a_venv();
    let project_directory = std::fs::canonicalize(project.path()).unwrap();
    let folder_name = project_directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let queued = runner
        .create_project(CreateProjectRequest {
            project_directory: Some(project.path().to_path_buf()),
            sample: None,
        })
        .expect("a create of a project is queued");

    assert_eq!(queued.project_directory, project_directory);
    let folder_cast = crate::core::graph::cast_exposed_name_to_url_safe(&folder_name).unwrap();
    assert!(
        queued
            .stream_id
            .starts_with(&format!("{folder_cast}-main-")),
        "{}",
        queued.stream_id
    );
    let done = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(done.kind, StreamActionKind::Open);
    assert_eq!(
        done.streams
            .iter()
            .map(|stream| (stream.stream_id.clone(), stream.display_name.clone()))
            .collect::<Vec<_>>(),
        [(queued.stream_id.clone(), folder_name.clone())]
    );
    for scaffolded_file in [
        PROJECT_FILE_NAME,
        "streams/main.graph.json",
        "pyproject.toml",
        ".python-version",
        ".gitignore",
    ] {
        assert!(
            project_directory.join(scaffolded_file).is_file(),
            "{scaffolded_file}"
        );
    }
    assert!(!project_directory.join("stream.py").exists());
    let project_file = TatolabProjectFile::read_from_project_directory(&project_directory)
        .expect("the project file reads");
    assert_eq!(project_file.name, folder_name);
    let record = the_record_of(state_directory.path(), &queued.stream_id);
    assert_eq!(
        the_display_name_of_the_stream_at(&record.location, &record.stream_id),
        folder_name,
        "the scaffold's graph document shows its stream under the folder's name"
    );
    assert_eq!(record.location.project, project_file.project);
    assert_eq!(*first_commits.lock(), [project_directory]);
}

#[test]
#[serial]
fn a_create_over_a_scaffold_file_fails_its_action_with_nothing_written() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = a_directory_holding_only_a_venv();
    std::fs::create_dir_all(project.path().join("streams")).unwrap();
    std::fs::write(
        project.path().join("streams/main.graph.json"),
        "# the user's own\n",
    )
    .unwrap();

    let queued = runner
        .create_project(CreateProjectRequest {
            project_directory: Some(project.path().to_path_buf()),
            sample: None,
        })
        .expect("a create over a scaffold file is queued, and its action fails");

    let failed = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
    assert!(
        failed
            .failed_because
            .as_deref()
            .is_some_and(|why| why.contains("already has streams/main.graph.json")),
        "{failed:?}"
    );
    assert!(!project.path().join("pyproject.toml").exists());
    assert!(!project.path().join(PROJECT_FILE_NAME).exists());
    assert_eq!(
        std::fs::read_to_string(project.path().join("streams/main.graph.json")).unwrap(),
        "# the user's own\n"
    );
    assert!(runner.list_streams().is_empty());
}

#[test]
#[serial]
fn a_create_in_a_relative_directory_is_refused_with_nothing_created() {
    let runner = a_runner_recording_its_streams_in(None);
    let refusal = runner
        .create_project(CreateProjectRequest {
            project_directory: Some(PathBuf::from("a-relative-project")),
            sample: None,
        })
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("must be an absolute path"), "{refusal}");
    assert!(!Path::new("a-relative-project").exists());
}

/// The stream asset UUID of the sample `lens` a test runtime unit carries.
const SAMPLE_MAIN_STREAM_ASSET_FOR_TEST: &str = "0eaf9ee2-0f73-4154-a3f6-a99c261b850d";

/// A runner whose runtime unit, in the returned folder, carries the samples `lens` and
/// `chord` beside its lend, each a project whose `streams/main.graph.json` holds one node.
fn a_runner_whose_unit_carries_two_samples(
    streams_directory: &Path,
) -> (Arc<Runner>, tempfile::TempDir) {
    ensure_test_mocks_registered();
    let runtime_unit_root = tempfile::tempdir().unwrap();
    let lend_directory = runtime_unit_root.path().join("lib/tatolab/lend");
    std::fs::create_dir_all(&lend_directory).unwrap();
    let samples_directory = sample_projects_directory_beside_the_lend(&lend_directory).unwrap();
    for (sample_name, title) in [("lens", "Halftone lens"), ("chord", "A chord")] {
        let sample_folder = samples_directory.join(sample_name);
        std::fs::create_dir_all(sample_folder.join("streams")).unwrap();
        std::fs::create_dir_all(sample_folder.join("nodes")).unwrap();
        let mut project_file = TatolabProjectFile::of_a_new_project(title);
        project_file.description = Some(format!("{title}, as a sample."));
        std::fs::write(
            sample_folder.join(PROJECT_FILE_NAME),
            project_file.canonical_bytes().unwrap(),
        )
        .unwrap();
        let mut main_graph = the_graph_of_a_stream_named("main");
        main_graph["asset"] = serde_json::json!(SAMPLE_MAIN_STREAM_ASSET_FOR_TEST);
        main_graph["nodes"][0]["id"] = serde_json::json!(Uuid::new_v4());
        std::fs::write(
            sample_folder.join("streams/main.graph.json"),
            serde_json::to_vec_pretty(&main_graph).unwrap(),
        )
        .unwrap();
        std::fs::write(
            sample_folder.join("pyproject.toml"),
            format!("[project]\nname = \"{sample_name}\"\n"),
        )
        .unwrap();
        std::fs::write(sample_folder.join("nodes/lens.py"), "LENS = 1\n").unwrap();
    }
    let runner = Runner::new().expect("the runner builds");
    runner
        .set_node_interpreter_lend_directory(lend_directory)
        .expect("the lend directory is handed over");
    runner
        .record_streams_in_the_state_directory(streams_directory)
        .expect("the runner records its streams there");
    (runner, runtime_unit_root)
}

#[test]
#[serial]
fn the_samples_the_runtime_unit_carries_are_listed_in_name_order() {
    let state_directory = tempfile::tempdir().unwrap();
    let (runner, _runtime_unit_root) =
        a_runner_whose_unit_carries_two_samples(state_directory.path());

    assert_eq!(
        runner.list_samples(),
        ListSamplesToolResult {
            samples: vec![
                ListSamplesToolResultSample {
                    name: "chord".to_owned(),
                    title: "A chord".to_owned(),
                    summary: "A chord, as a sample.".to_owned(),
                },
                ListSamplesToolResultSample {
                    name: "lens".to_owned(),
                    title: "Halftone lens".to_owned(),
                    summary: "Halftone lens, as a sample.".to_owned(),
                },
            ],
        }
    );
    assert!(
        a_runner_recording_its_streams_in(None)
            .list_samples()
            .samples
            .is_empty(),
        "a lend with no samples beside it lists none"
    );
}

#[test]
#[serial]
fn a_create_from_a_sample_copies_it_under_a_new_project_and_loads_its_main_stream() {
    let state_directory = tempfile::tempdir().unwrap();
    let (runner, runtime_unit_root) =
        a_runner_whose_unit_carries_two_samples(state_directory.path());
    let first_commits = Arc::new(Mutex::new(Vec::new()));
    let first_commits_made = Arc::clone(&first_commits);
    runner
        .set_created_project_first_commit(Arc::new(move |project_directory: &Path| {
            first_commits_made
                .lock()
                .push(project_directory.to_path_buf());
            Ok(())
        }))
        .unwrap();
    let project = a_directory_holding_only_a_venv();
    let project_directory = std::fs::canonicalize(project.path()).unwrap();
    let folder_name = project_directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let queued = runner
        .create_project(CreateProjectRequest {
            project_directory: Some(project.path().to_path_buf()),
            sample: Some("lens".to_owned()),
        })
        .expect("a create from a sample is queued");

    let done = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(
        done.streams
            .iter()
            .map(|stream| (stream.stream_id.clone(), stream.display_name.clone()))
            .collect::<Vec<_>>(),
        [(queued.stream_id.clone(), folder_name.clone())]
    );
    assert_eq!(
        std::fs::read_to_string(project_directory.join("nodes/lens.py")).unwrap(),
        "LENS = 1\n"
    );
    assert_eq!(
        std::fs::read_to_string(project_directory.join("pyproject.toml")).unwrap(),
        "[project]\nname = \"lens\"\n",
        "a sample's pyproject.toml is copied as the sample writes it"
    );
    let project_file = TatolabProjectFile::read_from_project_directory(&project_directory)
        .expect("the project file reads");
    let sample_project_file = TatolabProjectFile::read_from_project_directory(
        &runtime_unit_root.path().join("lib/tatolab/samples/lens"),
    )
    .unwrap();
    assert_ne!(project_file.project, sample_project_file.project);
    assert_eq!(project_file.name, folder_name);
    assert_eq!(
        project_file.description.as_deref(),
        Some("Halftone lens, as a sample.")
    );
    let record = the_record_of(state_directory.path(), &queued.stream_id);
    assert_eq!(record.location.project, project_file.project);
    assert_eq!(
        record.location.stream_asset.to_string(),
        SAMPLE_MAIN_STREAM_ASSET_FOR_TEST
    );
    assert_eq!(
        the_display_name_of_the_stream_at(&record.location, &record.stream_id),
        folder_name
    );
    assert_eq!(*first_commits.lock(), [project_directory]);
}

#[test]
#[serial]
fn a_create_from_a_sample_the_unit_does_not_carry_is_refused_naming_the_ones_it_does() {
    let state_directory = tempfile::tempdir().unwrap();
    let (runner, _runtime_unit_root) =
        a_runner_whose_unit_carries_two_samples(state_directory.path());
    let project = a_directory_holding_only_a_venv();

    let refusal = runner
        .create_project(CreateProjectRequest {
            project_directory: Some(project.path().to_path_buf()),
            sample: Some("cube".to_owned()),
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.contains("the runtime unit carries no sample `cube`; it carries chord, lens"),
        "{refusal}"
    );
    assert!(!project.path().join(PROJECT_FILE_NAME).exists());
    assert!(runner.stream_actions_under_way().is_empty());
}

#[test]
#[serial]
fn an_open_project_answers_its_action_at_once_and_the_action_walks_to_done_holding_its_streams_stopped()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.hold_the_streams(&["camera", "preview"], &[]);
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let project_directory = std::fs::canonicalize(project.path()).unwrap();

    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .expect("an open of a project is queued");

    assert_eq!(queued.project_directory, project_directory);
    let making_the_venv = the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });
    assert_eq!(making_the_venv.state, StreamActionState::InProgress);
    assert_eq!(making_the_venv.kind, StreamActionKind::Open);
    let mut streams_preparing: Vec<StreamListing> = runner
        .list_streams()
        .into_iter()
        .filter(|listing| listing.state == StreamListingState::Preparing)
        .collect();
    streams_preparing.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
    let mut expected_preparing = vec![
        project.stream_id_of("camera"),
        project.stream_id_of("preview"),
    ];
    expected_preparing.sort();
    assert_eq!(
        streams_preparing
            .iter()
            .map(|listing| listing.stream_id.clone())
            .collect::<Vec<_>>(),
        expected_preparing,
        "each graph document's stream is listed preparing while the venv is made"
    );
    for listing in &streams_preparing {
        assert_eq!(
            listing.stream_action_under_way,
            Some(StreamActionWorkingOnAStream {
                action_id: queued.action_id.clone(),
                step: Some(StreamActionStep::MakingTheVenv),
            })
        );
    }

    std::fs::write(&gate, "").unwrap();
    let done = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(done.step, None);
    assert_eq!(
        done.streams
            .iter()
            .map(|stream| (stream.stream_id.clone(), stream.state))
            .collect::<Vec<_>>(),
        [
            (
                project.stream_id_of("camera"),
                Some(StreamListingState::Stopped)
            ),
            (
                project.stream_id_of("preview"),
                Some(StreamListingState::Stopped)
            ),
        ]
    );
    assert!(
        runner
            .list_streams()
            .iter()
            .all(|listing| listing.state == StreamListingState::Stopped
                && listing.node_count.is_some()
                && listing.stream_action_under_way.is_none()),
        "{:?}",
        runner.list_streams()
    );
    assert_eq!(stub_uv.runs().len(), 1, "{:?}", stub_uv.runs());
    assert!(
        runner
            .every_loaded_stream()
            .iter()
            .all(|held| held.is_held_stopped() && held.runtime_context.lock().is_none()),
        "an open starts nothing"
    );
    assert!(the_record_of(state_directory.path(), &project.stream_id_of("camera")).stopped);
}

#[test]
#[serial]
fn a_closed_project_unloads_its_streams_records_them_closed_and_a_fresh_runtime_leaves_them_unloaded()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);

    let queued = runner
        .close_project(project.path())
        .expect("a close of an open project is queued");
    let closed = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(closed.state, StreamActionState::Done, "{closed:?}");
    assert_eq!(closed.kind, StreamActionKind::Close);
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    let record = the_record_of(state_directory.path(), &camera);
    assert!(record.closed && record.stopped, "{record:?}");
    assert!(record.pending_stream_actions.is_empty(), "{record:?}");
    assert!(
        project.path().join(".venv").exists(),
        "a close erases nothing it synced"
    );
    assert_eq!(runner.list_streams()[0].state, StreamListingState::Closed);
    let refusal = refusal_of(runner.close_project(project.path()));
    assert!(refusal.contains("is not open"), "{refusal}");

    let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    assert!(
        fresh_runner
            .reload_every_recorded_stream_not_closed_at_once()
            .is_empty()
    );
    assert!(fresh_runner.ids_of_the_loaded_streams().is_empty());
    assert_eq!(
        fresh_runner.list_streams()[0].state,
        StreamListingState::Closed
    );
}

#[test]
#[serial]
fn a_reload_of_an_open_stopped_project_keeps_its_streams_held_stopped() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    a_project_opened_on(&runner, &project);

    let queued = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .expect("a reload of an open project is queued");
    let reloaded = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(reloaded.state, StreamActionState::Done, "{reloaded:?}");
    assert_eq!(reloaded.kind, StreamActionKind::Load);
    assert!(
        reloaded
            .streams
            .iter()
            .all(|stream| stream.state == Some(StreamListingState::Stopped)),
        "{reloaded:?}"
    );
    let held = runner.every_loaded_stream();
    assert!(!held.is_empty());
    assert!(
        held.iter()
            .all(|held| held.is_held_stopped() && held.runtime_context.lock().is_none()),
        "a reload starts nothing"
    );
}

/// The graph of a stream named `stream_name` holding two nodes, `source` and `second`.
fn the_graph_of_a_stream_named_holding_a_second_node(stream_name: &str) -> serde_json::Value {
    let mut graph = the_graph_of_a_stream_named(stream_name);
    let source = graph["nodes"][0].clone();
    let mut second = source.clone();
    second["name"] = serde_json::json!("second");
    graph["nodes"] = serde_json::json!([source, second]);
    graph
}

#[test]
#[serial]
fn a_reload_with_the_node_code_unchanged_keeps_the_loaded_stream_and_reconciles_it_to_its_document()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let opened = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert_eq!(opened.node_count(), 1);

    project.write_the_graph_document(the_graph_of_a_stream_named_holding_a_second_node("camera"));
    let reloaded = the_project_reloaded(&runner, &project);

    let kept = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(
        Arc::ptr_eq(&opened, &kept),
        "the stream is kept, not loaded again"
    );
    assert_eq!(kept.node_count(), 2, "the second node is reconciled in");
    assert!(kept.is_held_stopped() && kept.runtime_context.lock().is_none());
    assert_eq!(
        reloaded
            .streams
            .iter()
            .map(|stream| (stream.stream_id.as_str(), stream.state))
            .collect::<Vec<_>>(),
        [(camera.as_str(), Some(StreamListingState::Stopped))],
        "{reloaded:?}"
    );
}

#[test]
#[serial]
fn a_reload_after_an_edit_of_the_projects_node_code_loads_its_streams_again() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let opened = runner.loaded_stream_of_the_id(&camera).unwrap();

    std::fs::create_dir_all(project.path().join("nodes")).unwrap();
    std::fs::write(project.path().join("nodes/blur.py"), "class Blur: pass\n").unwrap();
    the_project_reloaded(&runner, &project);

    let loaded_again = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(
        !Arc::ptr_eq(&opened, &loaded_again),
        "the stream is loaded again"
    );
    assert!(loaded_again.is_held_stopped());
}

#[test]
#[serial]
fn a_reload_whose_document_no_longer_loads_records_the_kept_stream_failed() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);

    project.write_an_unloadable_graph_document("camera");
    the_project_reloaded(&runner, &project);

    assert!(
        runner
            .loaded_stream_of_the_id_when_loaded(&camera)
            .is_none()
    );
    let record = the_record_of(state_directory.path(), &camera);
    assert!(record.is_failed(), "{record:?}");
}

#[test]
#[serial]
fn a_reload_of_a_project_never_opened_is_refused_naming_open_project() {
    let runner = a_runner_recording_its_streams_in(None);
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

    let refusal =
        refusal_of(runner.reload_project(project_open_or_reload_request_of(project.path())));

    assert!(refusal.contains("`open_project`"), "{refusal}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn an_open_project_of_a_folder_that_is_no_project_fails_its_action_saying_why() {
    let runner = a_runner_recording_its_streams_in(None);
    let no_project = a_temporary_directory_at_owner_only_mode().unwrap();

    for (project_directory, why) in [
        (no_project.path().join("missing"), "does not exist"),
        (
            no_project.path().to_path_buf(),
            "is not a project: it holds no tatolab-project.json",
        ),
    ] {
        let queued = runner
            .open_project(project_open_or_reload_request_of(&project_directory))
            .expect("an open refuses nothing at the call");
        let failed = the_stream_action_once_finished(&runner, &queued.action_id);

        assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
        let failed_because = failed.failed_because.unwrap_or_default();
        assert!(failed_because.contains(why), "{failed_because}");
    }
    assert!(
        refusal_of(
            runner.open_project(project_open_or_reload_request_of(Path::new(
                "relative/desk"
            )))
        )
        .contains("`project_directory` must be an absolute path"),
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_queued_open_of_a_project_holding_no_graph_document_is_done_with_no_stream() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();

    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    let done = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert!(done.streams.is_empty(), "{done:?}");
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn a_stream_whose_project_is_preparing_is_held_from_its_document_alone_and_loads_the_document_as_edited()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .expect("an open of a project is queued");
    the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let streams = runner
        .the_streams_to_read_or_edit(std::slice::from_ref(&camera))
        .expect("a stream preparing is held from its document alone");
    let graph = streams
        .runtime_operations_of(&camera)
        .and_then(|held_alone| held_alone.to_json())
        .expect("the graph held from its document renders");
    assert_eq!(graph["nodes"].as_array().map(Vec::len), Some(1), "{graph}");
    assert!(
        runner
            .loaded_stream_of_the_id_when_loaded(&camera)
            .is_none(),
        "a stream held alone is never entered in the stream table"
    );
    drop(streams);

    let mut edited = the_graph_of_a_stream_named("camera");
    edited["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "second_source",
            "type": MockOutputOnlyNode::Node::node_class_import_path().as_str(),
            "config": {},
        }));
    project.write_the_graph_document(edited);
    std::fs::write(&gate, "").unwrap();
    let done = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .node_count(),
        2,
        "the load reads the document as it was edited while the project prepared"
    );
}

#[test]
#[serial]
fn a_load_waits_for_an_edit_made_through_a_stream_held_from_its_document() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .expect("an open of a project is queued");
    the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let streams_being_edited = runner
        .the_streams_to_read_or_edit(std::slice::from_ref(&camera))
        .expect("a stream preparing is held from its document alone");
    std::fs::write(&gate, "").unwrap();
    the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::Starting)
    });
    let mut edited = the_graph_of_a_stream_named("camera");
    edited["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "second_source",
            "type": MockOutputOnlyNode::Node::node_class_import_path().as_str(),
            "config": {},
        }));
    project.write_the_graph_document(edited);
    drop(streams_being_edited);
    let done = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .node_count(),
        2,
        "the load reads the document only once the edit made through the stream held from it \
         is written"
    );
}

#[test]
#[serial]
fn a_graph_document_given_another_documents_node_uuid_while_its_project_prepares_refuses_the_load()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .expect("an open of a project is queued");
    the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let camera_document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.graph_document_path_of("camera")).unwrap())
            .unwrap();
    let mut copied_by_hand = the_graph_of_a_stream_named("camera-copy");
    copied_by_hand["nodes"][0]["id"] = camera_document["nodes"][0]["id"].clone();
    project.write_the_graph_document(copied_by_hand);
    std::fs::write(&gate, "").unwrap();
    let failed = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
    let failed_because = failed.failed_because.unwrap_or_default();
    assert!(
        failed_because.contains("`streams/camera.graph.json`")
            && failed_because.contains("`streams/camera-copy.graph.json`"),
        "{failed_because}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_stream_of_a_closed_project_is_held_from_its_document_alone() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let queued = runner
        .close_project(project.path())
        .expect("a close of an open project is queued");
    the_stream_action_once_finished(&runner, &queued.action_id);

    let streams = runner
        .the_streams_to_read_or_edit(std::slice::from_ref(&camera))
        .expect("a stream of a closed project is held from its document alone");
    let graph = streams
        .runtime_operations_of(&camera)
        .and_then(|held_alone| held_alone.to_json())
        .expect("the graph held from its document renders");

    assert_eq!(graph["nodes"].as_array().map(Vec::len), Some(1), "{graph}");
    drop(streams);
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert_eq!(runner.list_streams()[0].state, StreamListingState::Closed);
}

#[test]
#[serial]
fn the_node_catalog_of_a_stream_of_a_closed_project_lists_the_native_types_reading_none_of_its_node_code()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let meter_view_folder = project
        .project_directory
        .join(PROJECT_VIEWS_DIRECTORY)
        .join("meter");
    std::fs::create_dir_all(&meter_view_folder).unwrap();
    std::fs::write(
        meter_view_folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME),
        serde_json::to_vec(&serde_json::json!({
            "contract": "1.0", "title": "Meter", "entry": "index.html", "placement": "node",
        }))
        .unwrap(),
    )
    .unwrap();
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let catalog_while_loaded = runner
        .node_catalog_of_the_stream_a_call_names(&camera)
        .expect("a loaded stream's catalog answers");
    let queued = runner
        .close_project(project.path())
        .expect("a close of an open project is queued");
    the_stream_action_once_finished(&runner, &queued.action_id);

    let catalog_while_closed = runner
        .node_catalog_of_the_stream_a_call_names(&camera)
        .expect("a stream of a closed project still answers its node catalog");

    let types_of = |catalog: &[NodeDescriptor]| -> BTreeSet<String> {
        catalog
            .iter()
            .map(|descriptor| descriptor.node_class_import_path.as_str().to_owned())
            .collect()
    };
    let types_while_closed = types_of(&catalog_while_closed);
    assert!(
        types_while_closed.contains(MockOutputOnlyNode::Node::node_class_import_path().as_str()),
        "the native type of the stream's node is listed"
    );
    assert!(
        types_while_closed.contains(&format!("{TEST_PROJECT_NAME}:meter")),
        "the view the stream's project brings is listed"
    );
    // Other tests may register native types meanwhile, so the catalog read later holds at least
    // every type of the one read while the stream was loaded.
    assert!(
        types_of(&catalog_while_loaded).is_subset(&types_while_closed),
        "the closed stream lists every type it listed while loaded"
    );
    assert!(
        runner.ids_of_the_loaded_streams().is_empty(),
        "reading the catalog leaves the stream unloaded"
    );
    assert_eq!(runner.list_streams()[0].state, StreamListingState::Closed);
}

#[test]
#[serial]
fn the_node_catalog_of_a_stream_this_runtime_does_not_hold_is_refused_naming_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let refusal = runner
        .node_catalog_of_the_stream_a_call_names("nowhere")
        .expect_err("a stream this runtime does not hold is refused");

    assert!(matches!(refusal, Error::NotFound(_)), "{refusal}");
    assert!(
        refusal
            .to_string()
            .contains("no stream with the id `nowhere`"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_stream_this_runtime_does_not_list_is_not_held_from_any_document() {
    let runner = a_runner_recording_its_streams_in(Some(tempfile::tempdir().unwrap().path()));

    let refusal = runner
        .the_streams_to_read_or_edit(&["nowhere".to_owned()])
        .err()
        .expect("a stream not listed is refused");

    assert!(matches!(refusal, Error::NotFound(_)), "{refusal}");
}
