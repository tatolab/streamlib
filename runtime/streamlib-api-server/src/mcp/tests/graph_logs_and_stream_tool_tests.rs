// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The graph, logs and stream tools: a stream's graph and the reason a link was refused, a
//! project's logs, the stream actions and the refusals they answer, and each listing state on the
//! wire.

use super::*;

#[tokio::test]
async fn tools_call_graph_naming_a_stream_returns_that_streams_graph() {
    let body = tool_call_result(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "graph",
        json!({ "stream": STUB_STREAM_ID }),
    )
    .await;
    let graph = first_text_block_json(&body);
    assert_eq!(graph, json!({ "nodes": [], "links": [] }));
}

/// Without `stream`, `graph` is every loaded stream's graph under the
/// machine's name — what an agent reads first to learn what is loaded.
#[tokio::test]
async fn tools_call_graph_without_a_stream_renders_every_loaded_stream_under_the_machines_name() {
    let runtime = ControlPlaneMcpDispatchStubRuntime::new();
    *runtime.exported_graph.lock() = two_linked_nodes_graph();

    let body = tool_call_result(Arc::new(runtime), "graph", json!({})).await;

    assert_eq!(
        first_text_block_json(&body),
        json!({
            "machine": STUB_MACHINE_NAME,
            "streams": [two_linked_nodes_graph()],
        })
    );
    let machine_wide: streamlib::sdk::json_schema::MachineWideGraphResponse =
        serde_json::from_value(first_text_block_json(&body))
            .expect("the machine-wide graph parses as the schema the generator writes");
    assert_eq!(machine_wide.streams.len(), 1);
}

#[tokio::test]
async fn tools_call_graph_naming_a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
    let body = tool_call_result(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "graph",
        json!({ "stream": "elsewhere" }),
    )
    .await;
    assert_eq!(body["isError"], true, "{body}");
    let text = body["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("elsewhere") && text.contains(STUB_STREAM_ID),
        "{text}"
    );
}

/// `logs` reads a project's log from disk, answering where to read on from,
/// and holds no record for a project that has written none.
#[tokio::test]
async fn tools_call_logs_answers_a_project_that_wrote_no_record_with_none() {
    let project_directory = tempfile::tempdir().unwrap();
    let body = tool_call_result(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "logs",
        json!({
            "project_directory": project_directory.path(),
            "stream": STUB_STREAM_ID,
            "count": 16,
        }),
    )
    .await;

    assert_eq!(
        first_text_block_json(&body),
        json!({
            "project_directory": project_directory.path(),
            "records": [],
            "next_after": "1:0",
            "records_no_longer_held": false,
        })
    );
}

#[tokio::test]
async fn tools_call_logs_refuses_a_call_naming_no_project_a_relative_one_or_a_malformed_cursor() {
    let project_directory = tempfile::tempdir().unwrap();
    for (arguments, what_the_refusal_must_name) in [
        (json!({ "stream": STUB_STREAM_ID }), "project_directory"),
        (json!({ "project_directory": "relative" }), "relative"),
        (
            json!({ "project_directory": project_directory.path(), "since": 3 }),
            "since",
        ),
        (
            json!({ "project_directory": project_directory.path(), "after": "seven" }),
            "seven",
        ),
    ] {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "logs",
            arguments.clone(),
        )
        .await;
        assert_eq!(body["isError"], true, "{arguments}: {body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(what_the_refusal_must_name),
            "{arguments} must be refused naming {what_the_refusal_must_name}: {text}"
        );
    }
}

/// The words the stub runtime refuses every stream action in: a refusal
/// carrying them reached the runtime, one without them was refused before.
const THE_STUB_RUNTIMES_STREAM_ACTION_REFUSAL: &str = "takes no stream action";

/// Every stream tool refuses an argument it does not take, a required one
/// missing, a relative project directory and a level that is not one of
/// the three, naming what is wrong, before any stream action runs.
#[tokio::test]
async fn every_stream_tool_refuses_malformed_arguments_before_any_stream_action() {
    let absolute_project_directory = "/home/someone/projects/camera";
    for (tool_name, arguments, what_the_refusal_must_name) in [
        (
            "open_project",
            json!({ "project_directory": absolute_project_directory, "keep": true }),
            "keep",
        ),
        ("open_project", json!({}), "project_directory"),
        (
            "open_project",
            json!({ "project_directory": absolute_project_directory, "name": "camera" }),
            "name",
        ),
        (
            "open_project",
            json!({ "project_directory": "projects/camera" }),
            "absolute",
        ),
        ("reload_project", json!({}), "project_directory"),
        (
            "reload_project",
            json!({ "project_directory": "projects/camera" }),
            "absolute",
        ),
        ("stop_playback", json!({}), "project_directory"),
        (
            "stop_playback",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        (
            "stop_playback",
            json!({ "project_directory": absolute_project_directory, "stream": STUB_STREAM_ID }),
            "stream",
        ),
        ("start_playback", json!({}), "project_directory"),
        (
            "start_playback",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        ("close_project", json!({}), "project_directory"),
        (
            "close_project",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        ("pause_playback", json!({}), "project_directory"),
        (
            "pause_playback",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        ("resume_playback", json!({}), "project_directory"),
        (
            "resume_playback",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        ("remove_project", json!({}), "project_directory"),
        (
            "remove_project",
            json!({ "project_directory": "relative/project" }),
            "absolute",
        ),
        (
            "remove_project",
            json!({ "project_directory": absolute_project_directory, "force": true }),
            "force",
        ),
        (
            "create_stream",
            json!({ "project_directory": absolute_project_directory, "path": "streams/a.graph.json" }),
            "display_name",
        ),
        (
            "set_stream_display_name",
            json!({ "project_directory": absolute_project_directory, "display_name": "Desk" }),
            "stream_asset",
        ),
        (
            "delete_stream",
            json!({ "project_directory": absolute_project_directory, "stream_asset": STUB_STREAM_ASSET, "stream": STUB_STREAM_ID }),
            "stream",
        ),
        (
            "list_streams",
            json!({ "stream": STUB_STREAM_ID }),
            "stream",
        ),
    ] {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            tool_name,
            arguments.clone(),
        )
        .await;
        assert_eq!(body["isError"], true, "{tool_name} {arguments}: {body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(what_the_refusal_must_name),
            "{tool_name} {arguments} must be refused naming {what_the_refusal_must_name}: \
             {text}"
        );
        assert!(
            !text.contains(THE_STUB_RUNTIMES_STREAM_ACTION_REFUSAL),
            "{tool_name} {arguments} reached the runtime: {text}"
        );
    }
}

/// A stream action the runtime refuses reaches the caller as a tool error
/// in the runtime's own words, nothing added.
#[tokio::test]
async fn a_stream_action_the_runtime_refuses_is_a_tool_error_in_its_own_words() {
    for (tool_name, arguments, action) in [
        (
            "open_project",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "open_project",
        ),
        (
            "reload_project",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "reload_project",
        ),
        (
            "stop_playback",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "stop_playback",
        ),
        (
            "start_playback",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "start_playback",
        ),
        (
            "close_project",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "close_project",
        ),
        (
            "pause_playback",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "pause_playback",
        ),
        (
            "resume_playback",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "resume_playback",
        ),
        (
            "remove_project",
            json!({ "project_directory": "/home/someone/projects/camera" }),
            "remove_project",
        ),
    ] {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            tool_name,
            arguments,
        )
        .await;
        assert_eq!(body["isError"], true, "{tool_name}: {body}");
        assert_eq!(
            body["content"][0]["text"],
            crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(action)
                .to_string(),
            "{tool_name}"
        );
    }
}

#[tokio::test]
async fn list_streams_answers_each_streams_id_display_name_location_state_node_count_and_failure() {
    for arguments in [json!({}), json!(null)] {
        let (_served, client) =
            connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let mut request = CallToolRequestParams::new("list_streams");
        if let Some(arguments) = arguments.as_object() {
            request = request.with_arguments(arguments.clone());
        }
        let body = wire_outcome(client.call_tool(request).await).unwrap();

        assert_eq!(
            first_text_block_json(&body),
            json!({
                "streams": [{
                    "stream": STUB_STREAM_ID,
                    "display_name": STUB_STREAM_ID,
                    "location": {
                        "project_directory": crate::control_plane_stub_support::STUB_PROJECT_DIRECTORY,
                        "project": crate::control_plane_stub_support::STUB_PROJECT,
                        "stream_asset": STUB_STREAM_ASSET,
                    },
                    "state": "running",
                    "node_count": null,
                    "failed_because": null,
                    "capture_devices_whose_access_was_never_asked_for": [],
                    "action": null,
                    "step": null,
                }]
            }),
            "arguments {arguments}"
        );
    }
}

/// The same graph after the helper that was to open the link refused it —
/// what a live `connect` onto a helper-placed node leaves behind when
/// that helper's port could not open.
///
/// Kept beside the wired fixture rather than replacing its link: the
/// prompts pick a link out of the graph they are rendered against, and an
/// errored one has no business in the recipes' happy path.
fn two_linked_nodes_graph_whose_link_a_helper_refused() -> Value {
    let mut graph = two_linked_nodes_graph();
    graph["links"][0]["state"] = json!("error");
    graph["links"][0]["error_reason"] = json!(
        "could not wire input port \"video\": BufferSizeExceedsMaxSupportedBufferSizeOfService"
    );
    graph
}

/// An agent is told to read a link's state, so a refused link has to reach
/// it with the helper's own reason rather than a bare `error`.
#[tokio::test]
async fn tools_call_graph_carries_the_reason_a_helper_refused_a_link_for() {
    let runtime = ControlPlaneMcpDispatchStubRuntime::new();
    *runtime.exported_graph.lock() = two_linked_nodes_graph_whose_link_a_helper_refused();

    let body = tool_call_result(
        Arc::new(runtime),
        "graph",
        json!({ "stream": STUB_STREAM_ID }),
    )
    .await;
    let text = body["content"][0]["text"].as_str().unwrap();
    let graph: Value = serde_json::from_str(text).unwrap();
    assert_eq!(graph["links"][0]["state"], "error");
    assert!(
        graph["links"][0]["error_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("BufferSizeExceeds")),
        "the helper's own reason is what tells an agent what to fix; got {graph}"
    );
}

#[test]
fn each_listing_state_reaches_the_wire_under_its_own_name() {
    for (engine_state, wire_spelling) in [
        (StreamListingState::Running, "running"),
        (StreamListingState::Stopped, "stopped"),
        (StreamListingState::Failed, "failed"),
    ] {
        assert_eq!(
            serde_json::to_value(listed_stream_state_on_the_wire(engine_state)).unwrap(),
            json!(wire_spelling)
        );
    }
}
