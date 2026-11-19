// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's node types, listed from each import's Python sources and announced on its feed as
//! they change.

use super::*;

/// A venv interpreter whose node type listing keeps the request it was sent in
/// `listing-request.json` and prints `listing.json`, which a test writes first.
pub(super) const STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING: &str = "#!/bin/sh\ncase \"$*\" in\n  *tatolab.stream._project_node_type_listing_entry*) cat > listing-request.json; cat listing.json ;;\nesac\nexit 0\n";

/// Project `cell` imports project `arm`: the listing is sent the Python files of `arm`'s folder
/// under `imports/` as they are on disk and nothing else of it, and answers each source as the
/// project's interpreter read it, under the project's UUID and name, the import's source named
/// as the project file in its folder names it.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_node_types_are_listed_with_each_imports_python_sources_in_its_folder() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::create_dir_all(arm.path().join("nodes")).expect("the arm's nodes directory");
    std::fs::write(
        arm.path().join("nodes/gripper.py"),
        "from tatolab.stream import node\n",
    )
    .expect("the arm's node code");
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let project_source = json!({
        "source": "project",
        "node_types": [{
            "type": "nodes.ik:InverseKinematics", "kind": "process", "description": "",
            "category": null, "inputs": [], "outputs": [], "path": "nodes/ik.py", "line": 4,
        }],
        "graph_generators": [],
        "unreadable_modules": [{
            "module": "nodes.broken", "path": "nodes/broken.py", "line": 2, "column": 7,
            "why": "SyntaxError: invalid syntax",
        }],
    });
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };
    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm is imported");
    let folder = added["dependency"]["folder"].clone();
    let version = added["dependency"]["version"].clone();
    let mut dependency_source = json!({
        "source": "dependency", "project": arm.project, "folder": folder, "version": version,
        "node_types": [], "graph_generators": [], "unreadable_modules": [],
    });
    std::fs::write(
        cell.path().join("listing.json"),
        json!({ "sources": [project_source, dependency_source] }).to_string(),
    )
    .expect("the listing document");

    let listed = call(
        "list_project_node_types",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the cell's node types are listed");

    assert_eq!(listed["project"], json!(cell.project), "{listed}");
    assert_eq!(listed["project_name"], TEST_PROJECT_NAME, "{listed}");
    dependency_source["project_name"] = json!(TEST_PROJECT_NAME);
    assert_eq!(
        listed["sources"],
        json!([project_source, dependency_source]),
        "{listed}"
    );
    let request: Value = serde_json::from_slice(
        &std::fs::read(cell.path().join("listing-request.json")).expect("the listing's request"),
    )
    .expect("the request is JSON");
    assert_eq!(
        request,
        json!({ "dependencies": [{
            "project": arm.project, "folder": folder, "version": version,
            "files": { "nodes/gripper.py": "from tatolab.stream import node\n" },
        }] })
    );
}

/// The next announcement on `change_feed` that names node types, within a few seconds.
async fn the_next_node_types_announced(
    change_feed: &mut tokio::sync::broadcast::Receiver<
        streamlib_project_repository::ProjectChangeFeedAnnouncement,
    >,
) -> streamlib_project_repository::ProjectChangeOnTheFeed {
    use streamlib_project_repository::ProjectChangeOnTheFeed;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let announced = change_feed.recv().await.expect("the feed carries on");
            if matches!(
                announced.change,
                ProjectChangeOnTheFeed::NodeTypesAdded(_)
                    | ProjectChangeOnTheFeed::NodeTypesRemoved(_)
                    | ProjectChangeOnTheFeed::NodeTypesUpdated(_)
            ) {
                return announced.change;
            }
        }
    })
    .await
    .expect("node types are announced")
}

fn the_node_types_named_by(
    change: &streamlib_project_repository::ProjectChangeOnTheFeed,
) -> (&'static str, Vec<String>) {
    use streamlib_project_repository::ProjectChangeOnTheFeed;
    let (kind, node_types) = match change {
        ProjectChangeOnTheFeed::NodeTypesAdded(node_types) => ("added", node_types),
        ProjectChangeOnTheFeed::NodeTypesRemoved(node_types) => ("removed", node_types),
        ProjectChangeOnTheFeed::NodeTypesUpdated(node_types) => ("updated", node_types),
        other => panic!("not a node type change: {other:?}"),
    };
    (
        kind,
        node_types
            .iter()
            .map(|offered| offered.node_type.node_type.clone())
            .collect(),
    )
}

/// Write the `listing.json` the stub interpreter prints for `project`: one installed distribution
/// offering `node_types`.
fn write_the_listing_of_one_distribution_offering(
    project: &ProjectWithGraphDocuments,
    node_types: Value,
) {
    std::fs::write(
        project.path().join("listing.json"),
        json!({ "sources": [{
            "source": "installed_distribution", "distribution": "tatolab-camera",
            "distribution_version": "0.68.90", "node_types": node_types,
            "graph_generators": [], "unreadable_modules": [],
        }] })
        .to_string(),
    )
    .expect("the listing document");
}

/// A node type as a listing document offers it, with no pins.
fn a_listed_node_type(node_type: &str, description: &str) -> Value {
    json!({ "type": node_type, "kind": "process", "description": description,
            "category": "Sources/Cameras", "inputs": [], "outputs": [] })
}

/// Once a client has listed a project's node types, a node file saved in its folder and its venv
/// made current each list them again, and the feed names each node type added, removed or
/// updated, carrying the source it comes from.
///
/// Mental-revert: with no follower nothing is announced after the save; with no diff every type is
/// announced as added on every listing.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_node_types_added_removed_and_updated_are_announced_on_its_feed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let handler = crate::mcp::LocalApiMcpServerHandler::new(
        engine.operations_on_the_loaded_streams(),
        tokio_util::sync::CancellationToken::new(),
    );
    let dispatcher = handler.control_operation_dispatcher();
    let _files_watched = dispatcher
        .project_repositories
        .watch_the_files_of(cell.project);
    let mut change_feed = dispatcher
        .project_repositories
        .subscribe_to_the_change_feed();
    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([
            a_listed_node_type("tatolab.camera:CameraSource", "a camera"),
            a_listed_node_type("tatolab.camera:LatencyProbe", "a probe"),
        ]),
    );
    dispatcher
        .list_project_node_types(streamlib_control_contract::ListProjectNodeTypesRequest {
            project_directory: cell.path().to_path_buf(),
        })
        .await
        .expect("the cell's node types are listed");

    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([
            a_listed_node_type("tatolab.camera:CameraSource", "a camera, now with HDR"),
            a_listed_node_type("tatolab.camera:LatencyProbe", "a probe"),
            a_listed_node_type("tatolab.camera:FrameCounter", "counts frames"),
        ]),
    );
    std::fs::write(cell.path().join("counter.py"), "x = 1\n").expect("a node file saved");

    let added = the_next_node_types_announced(&mut change_feed).await;
    assert_eq!(
        the_node_types_named_by(&added),
        ("added", vec!["tatolab.camera:FrameCounter".to_owned()])
    );
    let updated = the_next_node_types_announced(&mut change_feed).await;
    assert_eq!(
        the_node_types_named_by(&updated),
        ("updated", vec!["tatolab.camera:CameraSource".to_owned()])
    );
    let streamlib_project_repository::ProjectChangeOnTheFeed::NodeTypesUpdated(updated) = updated
    else {
        unreachable!()
    };
    assert_eq!(updated[0].node_type.description, "a camera, now with HDR");
    assert_eq!(
        serde_json::to_value(&updated[0].origin).expect("the origin serializes"),
        json!({ "source": "installed_distribution", "distribution": "tatolab-camera",
                "distribution_version": "0.68.90" })
    );

    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([
            a_listed_node_type("tatolab.camera:CameraSource", "a camera, now with HDR"),
            a_listed_node_type("tatolab.camera:FrameCounter", "counts frames"),
        ]),
    );
    dispatcher
        .project_repositories
        .announce_the_venv_made_current(cell.project);

    let removed = the_next_node_types_announced(&mut change_feed).await;
    assert_eq!(
        the_node_types_named_by(&removed),
        ("removed", vec!["tatolab.camera:LatencyProbe".to_owned()])
    );
}

/// A project's node types listed while its venv's interpreter does not answer the listing are
/// refused; once a stream action of the project has finished and the interpreter answers, the
/// project is listed again and the feed names every node type it offers as added.
///
/// Mental-revert: with the refusal not kept, or the follower not hearing stream actions, nothing
/// is announced after the action finishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_refused_its_node_type_listing_is_listed_again_once_an_action_of_it_finishes() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::clone(&operations_on_the_loaded_streams),
        tokio_util::sync::CancellationToken::new(),
    );
    let dispatcher = handler.control_operation_dispatcher();
    let mut change_feed = dispatcher
        .project_repositories
        .subscribe_to_the_change_feed();
    dispatcher
        .list_project_node_types(streamlib_control_contract::ListProjectNodeTypesRequest {
            project_directory: cell.path().to_path_buf(),
        })
        .await
        .expect_err("an interpreter that prints no listing is refused");

    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([a_listed_node_type(
            "tatolab.camera:CameraSource",
            "a camera"
        )]),
    );
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    the_project_action_once_finished(
        "open_project",
        &operations_on_the_loaded_streams,
        cell.path(),
    )
    .await;

    assert_eq!(
        the_node_types_named_by(&the_next_node_types_announced(&mut change_feed).await),
        ("added", vec!["tatolab.camera:CameraSource".to_owned()])
    );
}

/// Over a runtime whose stream action changes have stopped arriving, a node file saved in a
/// listed project's folder still lists it again.
///
/// Mental-revert: with the follower ending when the stream action changes close, nothing is
/// announced after the save.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_is_listed_again_after_a_change_once_stream_action_changes_stop_arriving() {
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::new(crate::mcp::tests::ControlPlaneMcpDispatchStubRuntime::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    let dispatcher = handler.control_operation_dispatcher();
    let _files_watched = dispatcher
        .project_repositories
        .watch_the_files_of(cell.project);
    let mut change_feed = dispatcher
        .project_repositories
        .subscribe_to_the_change_feed();
    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([a_listed_node_type(
            "tatolab.camera:CameraSource",
            "a camera"
        )]),
    );
    dispatcher
        .list_project_node_types(streamlib_control_contract::ListProjectNodeTypesRequest {
            project_directory: cell.path().to_path_buf(),
        })
        .await
        .expect("the cell's node types are listed");

    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([
            a_listed_node_type("tatolab.camera:CameraSource", "a camera"),
            a_listed_node_type("tatolab.camera:FrameCounter", "counts frames"),
        ]),
    );
    std::fs::write(cell.path().join("counter.py"), "x = 1\n").expect("a node file saved");

    assert_eq!(
        the_node_types_named_by(&the_next_node_types_announced(&mut change_feed).await),
        ("added", vec!["tatolab.camera:FrameCounter".to_owned()])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_listing_the_interpreter_does_not_print_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_project_node_types",
            json!({ "project_directory": cell.path() }),
        )
        .await,
    )
    .expect_err("an interpreter that prints nothing lists nothing");

    assert!(
        refusal.contains("list_project_node_types failed")
            && refusal.contains("printed no listing"),
        "{refusal}"
    );
}
