// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use crate::stream_document_upgrade::{upgrade_a_node_in_place, upgrade_stream_document_in_place};

fn upgraded(mut document: Value) -> Value {
    assert!(upgrade_stream_document_in_place(&mut document));
    document
}

fn a_link(source: &str, output: &str, target: &str, input: &str) -> Value {
    json!({"source": {"node": source, "port": output}, "target": {"node": target, "port": input}})
}

/// Mental-revert: without the row, each `{"node": ...}` stays a value of an input that takes a
/// wire, and no link reaches the input.
#[test]
fn each_saved_reference_becomes_a_link_from_the_connection_of_the_node_it_named() {
    let document = json!({
        "nodes": [
            {"id": "gateway-uuid", "name": "gateway", "type": "tatolab.moq:MoqGateway",
             "config": {"kind": "moq-relay"}},
            {"id": "track-uuid", "name": "track", "type": "tatolab.moq:MoqTrack",
             "config": {"gateway": {"node": "gateway"}, "track": "camera"}},
            {"id": "get-uuid", "name": "get-track", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "camera"}},
            {"id": "send-uuid", "name": "send", "type": "tatolab.moq:MoqTrackSend",
             "config": {"track": {"node": "get-track"}}},
            {"id": "receive-uuid", "name": "receive", "type": "tatolab.moq:MoqTrackReceive",
             "config": {"gateway": {"node": "gateway"}, "track": "camera"}},
        ],
        "links": [],
    });

    let upgraded = upgraded(document);

    assert_eq!(
        upgraded["nodes"][1]["input_values"],
        json!({"track": "camera"})
    );
    assert_eq!(upgraded["nodes"][3].get("input_values"), None);
    assert_eq!(
        upgraded["links"],
        json!([
            a_link("gateway-uuid", "connection", "track-uuid", "gateway"),
            a_link("get-uuid", "value", "send-uuid", "track"),
            a_link("gateway-uuid", "connection", "receive-uuid", "gateway"),
        ])
    );
}

/// Mental-revert: without the `SetVariable` row, the setter keeps its reference while its
/// getter is linked into the send, and the variable is refused for holding a reference a link
/// cannot carry.
#[test]
fn a_set_variable_holding_a_reference_to_a_track_is_linked_from_the_tracks_connection() {
    let document = json!({
        "nodes": [
            {"id": "track-uuid", "name": "track", "type": "tatolab.moq:MoqTrack",
             "input_values": {"track": "camera"}},
            {"id": "set-uuid", "name": "set-track", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "camera", "node_reference": {"node": "track"}}},
            {"id": "other-uuid", "name": "other", "type": "tatolab.camera:CameraSource"},
            {"id": "set-other-uuid", "name": "set-other", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "other", "node_reference": {"node": "other"}}},
        ],
        "links": [],
    });

    let upgraded = upgraded(document);

    assert_eq!(
        upgraded["nodes"][1]["input_values"],
        json!({"variable": "camera"})
    );
    assert_eq!(
        upgraded["nodes"][3]["input_values"],
        json!({"variable": "other", "node_reference": {"node": "other"}}),
        "a reference to a node that gives itself out no other way is kept"
    );
    assert_eq!(
        upgraded["links"],
        json!([a_link("track-uuid", "connection", "set-uuid", "value")])
    );
}

/// A reference to a node the level no longer holds names nothing to wire, so it is dropped, and
/// a node placed on its own drops every reference it held.
#[test]
fn a_reference_naming_no_node_of_the_level_or_on_a_node_placed_alone_is_dropped() {
    let document = json!({
        "nodes": [{"id": "track-uuid", "name": "track", "type": "tatolab.moq:MoqTrack",
                   "input_values": {"gateway": {"node": "gone"}, "track": "camera"}}],
    });
    let upgraded = upgraded(document);
    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({"track": "camera"})
    );
    assert_eq!(upgraded.get("links"), None);

    let mut node = json!({"name": "send", "type": "tatolab.moq:MoqTrackSend",
                          "input_values": {"track": {"node": "track"}}});
    assert!(upgrade_a_node_in_place(&mut node));
    assert_eq!(
        node,
        json!({"name": "send", "type": "tatolab.moq:MoqTrackSend"})
    );
}
