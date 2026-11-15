// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Labelled groups and canvas notes in a stream's graph document, through a reload and an undo.

use super::*;

/// A labelled group is made, edited and taken out, each as one transaction kept in the graph
/// document and rendered by `graph`; a fresh runtime loading the document renders it too, and
/// undo of its removal brings it back with every node that belonged to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_labelled_group_survives_a_reload_and_undo_of_its_removal_restores_its_nodes() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_group",
        json!({ "stream": stream_asset, "title": "sources", "description": "every camera",
                "colour": "#3b82f6", "nodes": [SOURCE_NODE_UUID],
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group is made");
    let group = added["added_groups"][0]["group"]
        .as_str()
        .expect("add_group answers the group's UUID")
        .to_owned();
    assert_eq!(added["added_groups"][0]["title"], "sources", "{added}");
    call(
        "set_group_fields",
        json!({ "stream": stream_asset, "group": group, "colour": "#22c55e",
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group's fields are set");

    let the_group_as = json!([{ "id": group, "title": "sources",
                                "description": "every camera", "colour": "#22c55e" }]);
    let source_of = |graph: &Value| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == SOURCE_NODE_UUID)
            .cloned()
            .expect("the graph holds the source")
    };
    let kept = project.the_graph_document_of("main");
    assert_eq!(kept["groups"], the_group_as, "{kept}");
    assert_eq!(source_of(&kept)["studio"]["group"], json!(group), "{kept}");
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["groups"], the_group_as, "{live}");
    assert_eq!(source_of(&live)["studio"]["group"], json!(group), "{live}");
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(reloaded["groups"], the_group_as, "{reloaded}");

    let removed = call(
        "remove_group",
        json!({ "stream": stream_asset, "group": group, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group is taken out");
    assert_eq!(
        removed["operations"][0]["nodes"],
        json!([SOURCE_NODE_UUID]),
        "{removed}"
    );
    let ungrouped = project.the_graph_document_of("main");
    assert!(ungrouped.get("groups").is_none(), "{ungrouped}");
    assert!(source_of(&ungrouped).get("studio").is_none(), "{ungrouped}");
    assert!(
        engine.the_live_graph_of(&stream_id).get("groups").is_none(),
        "the live graph still renders the group"
    );

    call(
        "undo",
        json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the undo applies");
    let restored = project.the_graph_document_of("main");
    assert_eq!(restored["groups"], the_group_as, "{restored}");
    assert_eq!(
        source_of(&restored)["studio"]["group"],
        json!(group),
        "{restored}"
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["groups"], the_group_as, "{live}");
    assert_eq!(source_of(&live)["studio"]["group"], json!(group), "{live}");
}

/// A canvas note is placed, edited and taken out, each as one transaction kept under the graph
/// document's `studio` and rendered by `graph`; a fresh runtime loading the document renders it
/// too, and undo of its removal brings it back. A node's studio fields are held to the studio
/// contract, a key outside it refused by name.
#[tokio::test(flavor = "multi_thread")]
async fn a_canvas_note_survives_a_reload_and_a_node_studio_key_outside_the_contract_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_note",
        json!({ "stream": stream_asset, "text": "check the exposure",
                "position": [40, 300], "size": [240, 120],
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note is placed");
    let note = added["added_notes"][0]["note"]
        .as_str()
        .expect("add_note answers the note's UUID")
        .to_owned();
    call(
        "set_note_fields",
        json!({ "stream": stream_asset, "note": note, "text": "check the white balance",
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note's fields are set");

    let the_studio_as = json!({ "contract": 1, "notes": [{
        "id": note, "text": "check the white balance",
        "position": [40, 300], "size": [240, 120] }] });
    assert_eq!(
        project.the_graph_document_of("main")["studio"],
        the_studio_as
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["studio"], the_studio_as, "{live}");
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(reloaded["studio"], the_studio_as, "{reloaded}");

    call(
        "remove_note",
        json!({ "stream": stream_asset, "note": note, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note is taken out");
    assert!(
        project
            .the_graph_document_of("main")
            .get("studio")
            .is_none()
    );
    assert!(engine.the_live_graph_of(&stream_id).get("studio").is_none());
    call(
        "undo",
        json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the undo applies");
    assert_eq!(
        project.the_graph_document_of("main")["studio"],
        the_studio_as
    );
    assert_eq!(
        engine.the_live_graph_of(&stream_id)["studio"],
        the_studio_as
    );

    let refusal = call(
        "set_studio_fields",
        json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                "studio": { "position": [10, 20], "zoom": 2 } }),
    )
    .await
    .expect_err("a key outside the studio contract is refused");
    assert!(refusal.contains("`studio.zoom`"), "{refusal}");
    call(
        "set_studio_fields",
        json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                "studio": { "position": [10, 20], "metadata": { "zoom": 2 } } }),
    )
    .await
    .expect("a client's own key under `metadata` is kept");
    let source = project.the_graph_document_of("main")["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == SOURCE_NODE_UUID)
        .cloned()
        .unwrap();
    assert_eq!(
        source["studio"],
        json!({ "contract": 1, "position": [10, 20], "metadata": { "zoom": 2 } })
    );
}
