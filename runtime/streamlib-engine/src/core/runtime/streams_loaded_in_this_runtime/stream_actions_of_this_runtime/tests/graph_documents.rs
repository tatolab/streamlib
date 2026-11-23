// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_node_uuids_a_project_holds_take_each_placed_streams_nodes_under_their_placements_uuids() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let main =
        crate::core::runtime::a_project_on_disk_for_a_test::the_location_of_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        );
    let held_stream_asset = Uuid::new_v4();
    let node_inside_the_placed_stream = Uuid::new_v4();
    let placement_of_the_held_stream = Uuid::new_v4();
    let streams_directory = project_directory.path().join("streams");
    std::fs::create_dir_all(&streams_directory).expect("a streams directory");
    std::fs::write(
        streams_directory.join("held.graph.json"),
        serde_json::json!({ "asset": held_stream_asset, "links": [], "nodes": [{
            "id": node_inside_the_placed_stream, "name": "lfo", "type": "nodes.lfo:Lfo" }] })
        .to_string(),
    )
    .expect("the held stream's document");
    std::fs::write(
        streams_directory.join("main.graph.json"),
        serde_json::json!({ "asset": main.stream_asset, "links": [], "nodes": [{
            "id": placement_of_the_held_stream, "name": "held",
            "type": streamlib_project_repository::STREAM_FRAGMENT_NODE_TYPE,
            "input_values": { "stream": { "asset": held_stream_asset } } }] })
        .to_string(),
    )
    .expect("the placing stream's document");

    let node_uuids = crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::every_node_uuid_the_graph_documents_of_the_project_hold(
        project_directory.path(),
    )
    .expect("every document reads and resolves");
    assert!(node_uuids.contains(&Uuid::new_v5(
        &placement_of_the_held_stream,
        node_inside_the_placed_stream.as_bytes(),
    )));
}

#[test]
#[serial]
fn a_graph_document_that_does_not_load_is_recorded_failed_naming_the_document() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_an_unloadable_graph_document("camera");
    let camera = project.stream_id_of("camera");

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a document that does not load is a failed stream, not a refused run");

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains("`streams/camera.graph.json` does not load"),
        "{failed_because}"
    );
    assert_eq!(stream.stream_id, camera);
    assert_eq!(
        stream.display_name, "camera",
        "shown under its graph document's name"
    );
    assert_eq!(stream.stream_location, project.location_of("camera"));
    assert!(!stream.replaced_a_recorded_stream);
    assert_eq!(run.project_directory, project.path());
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    let recorded = the_record_of(state_directory.path(), &camera);
    assert_eq!(recorded.location, project.location_of("camera"));
    assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
    let listed = runner.list_streams();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].display_name, "camera");
    assert_eq!(listed[0].state, StreamListingState::Failed);
    assert_eq!(listed[0].failed_because.as_deref(), Some(failed_because));
}

#[test]
#[serial]
fn a_graph_document_holding_an_integer_wider_than_64_bits_is_recorded_failed_naming_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let mut graph = the_graph_of_a_stream_named("camera");
    graph["nodes"][0]["input_values"] = serde_json::json!({ "value": "the wide integer" });
    project.write_the_graph_document(graph);
    let document_path = project.graph_document_path_of("camera");
    let document_text = std::fs::read_to_string(&document_path).unwrap();
    std::fs::write(
        &document_path,
        document_text.replace("\"the wide integer\"", "18446744073709551616"),
    )
    .unwrap();

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("an integer wider than 64 bits fails its stream, not the run");

    let (_, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains("`streams/camera.graph.json` does not load")
            && failed_because.contains("18446744073709551616"),
        "{failed_because}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_graph_document_holding_a_node_with_no_uuid_is_recorded_failed_naming_the_node() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let mut graph = the_graph_of_a_stream_named("camera");
    graph["nodes"][0]["id"] = serde_json::json!("not-a-uuid");
    project.write_the_graph_document(graph);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a node with no UUID fails its stream, not the run");

    let (_, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because
            .contains("`streams/camera.graph.json` holds no UUID as the `id` of `source`"),
        "{failed_because}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_project_two_of_whose_graph_documents_share_a_node_uuid_is_refused_naming_both() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera_document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.graph_document_path_of("camera")).unwrap())
            .unwrap();
    let mut copied_by_hand = the_graph_of_a_stream_named("camera-copy");
    copied_by_hand["nodes"][0]["id"] = camera_document["nodes"][0]["id"].clone();
    project.write_the_graph_document(copied_by_hand);

    let refusal = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect_err("two streams holding one node UUID refuse the load")
        .to_string();

    assert!(
        refusal.contains("`streams/camera.graph.json`")
            && refusal.contains("`streams/camera-copy.graph.json`")
            && refusal.contains(camera_document["nodes"][0]["id"].as_str().unwrap()),
        "{refusal}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_stream_placed_twice_in_one_stream_and_again_in_another_shares_no_node_uuid() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("look"));
    let placing_look = |placement_name: &str| {
        serde_json::json!({
            "name": placement_name,
            "type": streamlib_project_repository::STREAM_FRAGMENT_NODE_TYPE,
            "input_values": { "stream": { "asset": project.stream_asset_of("look") } },
        })
    };
    for (stream_name, placement_names) in [
        ("main", &["look-a", "look-b"][..]),
        ("other", &["look"][..]),
    ] {
        let mut placing_stream = the_graph_of_a_stream_named(stream_name);
        for placement_name in placement_names {
            placing_stream["nodes"]
                .as_array_mut()
                .unwrap()
                .push(placing_look(placement_name));
        }
        project.write_the_graph_document(placing_stream);
    }

    runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("each placement's nodes hold UUIDs of their own");

    let mut loaded = runner.ids_of_the_loaded_streams();
    loaded.sort();
    let mut placing = vec![project.stream_id_of("main"), project.stream_id_of("other")];
    placing.sort();
    assert_eq!(loaded, placing);
}

fn the_graph_of_a_stream_reading_the_image_asset(
    stream_name: &str,
    image: Uuid,
) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_name,
        "nodes": [{
            "name": "overlay",
            "type": crate::core::test_support::MockSourceReadingAnImageAsset::Node::node_class_import_path().as_str(),
            "input_values": { "image": { "asset": image } },
        }],
        "links": [],
    })
}

#[test]
#[serial]
fn a_node_referencing_an_asset_the_project_does_not_hold_fails_its_stream_naming_both() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let image = Uuid::new_v4();
    project.write_the_graph_document(the_graph_of_a_stream_reading_the_image_asset(
        "camera", image,
    ));

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a reference to no asset fails its stream, not the run");

    let (_, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains(&format!(
            "has node `overlay` whose input values reference the asset {image}, which the \
             project does not hold"
        )),
        "{failed_because}"
    );
}

#[test]
#[serial]
fn a_node_referencing_an_imported_image_asset_loads() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let image = streamlib_project_repository::ProjectAsset {
        asset: Uuid::new_v4(),
        kind: streamlib_project_repository::ProjectAssetKind::Media,
        path: streamlib_project_repository::ProjectRelativeAssetPath::parse("media/logo.png")
            .unwrap(),
        content_sha256: "0".repeat(64),
        content_byte_len: 0,
        import_settings: serde_json::Map::new(),
    };
    let sidecar = project.path().join(image.path.sidecar_path());
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, image.sidecar_bytes().unwrap()).unwrap();
    project.write_the_graph_document(the_graph_of_a_stream_reading_the_image_asset(
        "camera",
        image.asset,
    ));

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    assert!(
        matches!(
            run.streams.as_slice(),
            [StreamLoadedOrFailedByARun {
                state: StreamStateAfterItsRun::Running { .. },
                ..
            }]
        ),
        "{run:?}"
    );
}

#[test]
#[serial]
fn a_graph_document_naming_no_stream_asset_is_recorded_failed_under_one_derived_from_its_path() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document_bytes("camera", "{\"nodes\": [], \"links\": []}");

    let first = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let second = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, failed_because) = the_one_failed_stream_of(&first);
    assert!(
        failed_because.contains("names no stream asset"),
        "{failed_because}"
    );
    assert_eq!(
        the_one_failed_stream_of(&second).0.stream_id,
        stream.stream_id
    );
    assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
}

#[test]
#[serial]
fn a_running_stream_whose_graph_document_no_longer_loads_is_unloaded_and_recorded_failed() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let running = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    project.write_an_unloadable_graph_document("camera");

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(failed_because.contains("does not load"), "{failed_because}");
    assert!(stream.replaced_a_recorded_stream);
    assert!(running.has_ended(), "the old version is not running");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(
        the_record_of(state_directory.path(), &camera)
            .failed_because
            .is_some()
    );
}

#[test]
#[serial]
fn a_project_whose_graph_documents_are_gone_unloads_and_forgets_its_streams() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        std::slice::from_ref(&camera)
    );
    project.remove_the_graph_document("camera");

    let emptied = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    assert!(emptied.streams.is_empty());
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert_eq!(the_current_record_in(state_directory.path(), &camera), None);
}

#[test]
#[serial]
fn a_stream_another_stream_of_the_project_places_runs_only_inside_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.hold_the_streams(&["look", "main"], &[]);
    runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("both streams load while neither places the other");
    assert_eq!(runner.ids_of_the_loaded_streams().len(), 2);

    let mut main_placing_look = the_graph_of_a_stream_named("main");
    main_placing_look["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "look",
            "type": streamlib_project_repository::STREAM_FRAGMENT_NODE_TYPE,
            "input_values": { "stream": { "asset": project.stream_asset_of("look") } },
        }));
    project.write_the_graph_document(main_placing_look);
    let rerun = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("the project loads with main placing look");

    let main = project.stream_id_of("main");
    let streams_the_rerun_loaded: Vec<&str> = rerun
        .streams
        .iter()
        .map(|stream| stream.stream_id.as_str())
        .collect();
    assert_eq!(streams_the_rerun_loaded, [main.as_str()]);
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        std::slice::from_ref(&main)
    );
    assert_eq!(
        the_current_record_in(state_directory.path(), &project.stream_id_of("look")),
        None,
        "look is no stream of its own once main places it"
    );
    let main_graph = runner
        .loaded_stream_of_the_id(&main)
        .unwrap()
        .to_json()
        .unwrap();
    let look_in_main = main_graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "look")
        .unwrap_or_else(|| panic!("main places look: {main_graph}"));
    let names_inside_look: Vec<&str> = look_in_main["fragment"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert_eq!(names_inside_look, ["source"], "{look_in_main}");
}

#[test]
#[serial]
fn a_run_takes_in_every_graph_document_and_forgets_a_stream_whose_document_is_gone() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.hold_the_streams(&["camera", "preview"], &["broken"]);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("every graph document of the project is taken in");

    let states: Vec<(&str, bool)> = run
        .streams
        .iter()
        .map(|stream| {
            (
                stream.stream_id.as_str(),
                matches!(stream.state, StreamStateAfterItsRun::Running { .. }),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            (project.stream_id_of("broken").as_str(), false),
            (project.stream_id_of("camera").as_str(), true),
            (project.stream_id_of("preview").as_str(), true),
        ],
        "each document in path order"
    );
    let mut loaded = runner.ids_of_the_loaded_streams();
    loaded.sort();
    let mut expected_loaded = vec![
        project.stream_id_of("camera"),
        project.stream_id_of("preview"),
    ];
    expected_loaded.sort();
    assert_eq!(loaded, expected_loaded);
    assert_eq!(records_in(state_directory.path()).read_every().len(), 3);

    project.hold_the_streams(&["camera"], &["broken"]);
    let rerun = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    assert!(
        rerun
            .streams
            .iter()
            .all(|stream| stream.stream_id != project.stream_id_of("preview")),
        "{rerun:?}"
    );
    assert_eq!(
        the_current_record_in(state_directory.path(), &project.stream_id_of("preview")),
        None
    );
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        [project.stream_id_of("camera")]
    );
    assert_eq!(records_in(state_directory.path()).read_every().len(), 2);
}

#[test]
#[serial]
fn a_project_moved_to_another_directory_keeps_each_streams_id_and_display_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let mut graph_shown_as_front_camera = the_graph_of_a_stream_named("camera");
    graph_shown_as_front_camera["display_name"] = serde_json::json!("Front camera");
    let project = ProjectWithGraphDocuments::holding(graph_shown_as_front_camera);
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    runner
        .stop_the_stream_finishing_its_pending_stop(&camera, None)
        .unwrap();
    let moved_to = project.path().with_file_name(format!(
        "{}-moved",
        project.path().file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(project.path(), &moved_to).unwrap();
    let moved = ProjectWithGraphDocuments {
        project_directory: moved_to.clone(),
        project: project.project,
        _directory_removed_on_drop: Arc::clone(&project._directory_removed_on_drop),
    };

    let run = runner
        .load_the_project_off_its_queue(moved.run_request())
        .unwrap();

    let (stream, _, _) = the_one_running_stream_of(&run);
    assert_eq!(stream.stream_id, camera);
    assert_eq!(stream.display_name, "Front camera");
    assert_eq!(
        the_record_of(state_directory.path(), &camera)
            .location
            .project_directory,
        moved_to
    );
    let _ = std::fs::rename(&moved_to, project.path());
}
