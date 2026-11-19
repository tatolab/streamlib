// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `graph`'s answer from a real runtime reads as the contract types it: the TypeScript client
//! checks the answer as it reads it, every key the answer carries is one the contract states,
//! each input says where the value it reads comes from, and a link says when it converts.

use tokio::io::AsyncReadExt;

use super::input_value_set_tests::{
    InputValueTestNode, register_the_input_value_test_node_once, the_input_named,
};
use super::*;

const LIVE_MIXER_NODE_UUID: &str = "1c1c1c1c-1c1c-4c1c-8c1c-1c1c1c1c1c1c";
const SAVED_MIXER_NODE_UUID: &str = "2d2d2d2d-2d2d-4d2d-8d2d-2d2d2d2d2d2d";
const WIRED_MIXER_NODE_UUID: &str = "3e3e3e3e-3e3e-4e3e-8e3e-3e3e3e3e3e3e";
const COUNTED_MIXER_NODE_UUID: &str = "4f4f4f4f-4f4f-4f4f-8f4f-4f4f4f4f4f4f";

/// A project whose stream `main` holds four mixers, whose `gain` reads a value set live, a
/// saved one, what a link from the first mixer's Float `level` carries, and what one from its
/// Integer `count` carries, read for free; the first holds a config field.
fn a_project_whose_mixers_read_their_gain_from_each_place() -> ProjectWithGraphDocuments {
    register_the_input_value_test_node_once();
    let mixer = InputValueTestNode::node_class_import_path();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": LIVE_MIXER_NODE_UUID, "name": "live_mixer", "type": mixer.as_str(),
              "input_values": { "frame_width": 640 } },
            { "id": SAVED_MIXER_NODE_UUID, "name": "saved_mixer", "type": mixer.as_str(),
              "input_values": { "gain": 0.5 } },
            { "id": WIRED_MIXER_NODE_UUID, "name": "wired_mixer", "type": mixer.as_str() },
            { "id": COUNTED_MIXER_NODE_UUID, "name": "counted_mixer", "type": mixer.as_str() },
        ],
        "links": [
            { "source": { "node": LIVE_MIXER_NODE_UUID, "port": "level" },
              "target": { "node": WIRED_MIXER_NODE_UUID, "port": "gain" } },
            { "source": { "node": LIVE_MIXER_NODE_UUID, "port": "count" },
              "target": { "node": COUNTED_MIXER_NODE_UUID, "port": "gain" } },
        ],
    }))
}

/// What the TypeScript client read from `graph` of `stream_id` and of every loaded stream over
/// `socket_path`, and each key it found that the contract does not state.
async fn the_graph_the_typescript_client_reads(socket_path: &Path, stream_id: &str) -> Value {
    let fixture_script = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../sdk/tatolab-runtime-client/integration_fixtures/read_the_graph_and_check_every_key.mjs",
    );
    let mut typescript_client = tokio::process::Command::new("node")
        .arg(&fixture_script)
        .arg(socket_path)
        .arg(stream_id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("`node` runs the TypeScript client's integration fixture");
    let mut printed = String::new();
    let mut refused = String::new();
    let mut stdout = typescript_client.stdout.take().unwrap();
    let mut stderr = typescript_client.stderr.take().unwrap();
    let exited = tokio::time::timeout(Duration::from_secs(20), async {
        let (read_stdout, read_stderr) = tokio::join!(
            stdout.read_to_string(&mut printed),
            stderr.read_to_string(&mut refused)
        );
        read_stdout.unwrap();
        read_stderr.unwrap();
        typescript_client.wait().await.unwrap()
    })
    .await
    .expect("the TypeScript client reads both graphs and exits");
    assert!(
        exited.success(),
        "the TypeScript client read the graph as the contract types it: {refused}"
    );
    serde_json::from_str(&printed).unwrap()
}

/// An answer key the contract does not state, such as `input_values` left out of
/// the node's type, is printed as a key the contract does not state; a key typed otherwise than
/// served, such as `value_comes_from` missing a source, makes the client refuse the answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_runtimes_graph_reads_as_the_contract_types_it_and_names_where_each_value_comes_from()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_mixers_read_their_gain_from_each_place();
    let (stream_id, served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    let set_live = served
        .control_call(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": LIVE_MIXER_NODE_UUID,
                    "input": "gain", "value": 0.25, "mode": "live" }),
        )
        .await;
    assert_eq!(set_live["result"]["mode"], "live", "{set_live}");

    let read =
        the_graph_the_typescript_client_reads(&served.served.local_api_socket_path, &stream_id)
            .await;

    assert_eq!(
        read["keys_the_contract_does_not_state"],
        json!([]),
        "{read}"
    );
    let one_stream = &read["one_stream"];
    assert_eq!(one_stream["stream"], stream_id.as_str());
    assert!(
        read["every_stream"]["streams"]
            .as_array()
            .is_some_and(|streams| streams.iter().any(|graph| graph["stream"] == stream_id)),
        "{read}"
    );
    let live_mixer = the_node_named(one_stream, "live_mixer");
    assert_eq!(live_mixer["input_values"], json!({ "frame_width": 640 }));
    assert_eq!(live_mixer["config"], json!({ "frame_width": 640 }));
    let live_gain = the_input_named(&live_mixer, "gain");
    assert_eq!(
        (&live_gain["value"], &live_gain["value_comes_from"]),
        (&json!(0.25), &json!("live"))
    );
    assert_eq!(live_gain["pin_type"], json!({ "type": "float" }));
    assert_eq!(live_gain["flow"], "value");
    let video = the_input_named(&live_mixer, "video");
    assert!(
        video.get("value").is_none() && video.get("value_comes_from").is_none(),
        "an input that takes no stored value reads none: {video}"
    );
    let saved_mixer = the_node_named(one_stream, "saved_mixer");
    assert_eq!(saved_mixer["input_values"], json!({ "gain": 0.5 }));
    assert_eq!(saved_mixer["pins"], json!({ "gain": 0.5 }));
    let saved_gain = the_input_named(&saved_mixer, "gain");
    assert_eq!(
        (&saved_gain["value"], &saved_gain["value_comes_from"]),
        (&json!(0.5), &json!("saved"))
    );
    let wired_gain = the_input_named(&the_node_named(one_stream, "wired_mixer"), "gain");
    assert!(wired_gain.get("value").is_none(), "{wired_gain}");
    assert_eq!(wired_gain["value_comes_from"], "wire");
    let level_into_gain = the_link_into(one_stream, "wired_mixer");
    assert!(
        level_into_gain.get("free_conversion").is_none()
            && level_into_gain.get("warnings").is_none(),
        "a Float into a Float converts nothing and warns of nothing: {level_into_gain}"
    );
    let count_into_gain = the_link_into(one_stream, "counted_mixer");
    assert_eq!(
        count_into_gain["free_conversion"], true,
        "an Integer read as a Float converts for free: {count_into_gain}"
    );
    assert_eq!(
        count_into_gain["source"],
        json!({ "node": "live_mixer", "port": "count" })
    );
}

/// The link of `graph` into node `node`.
fn the_link_into<'graph>(graph: &'graph Value, node: &str) -> &'graph Value {
    graph["links"]
        .as_array()
        .and_then(|links| links.iter().find(|link| link["target"]["node"] == node))
        .unwrap_or_else(|| panic!("a link feeds `{node}`: {graph}"))
}
