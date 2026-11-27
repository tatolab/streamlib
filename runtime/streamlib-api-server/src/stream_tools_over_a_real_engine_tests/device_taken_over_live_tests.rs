// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A device held as a computed value is opened again by an input set while its stream plays,
//! with no stop and no node rebuilt: the new open takes the device over, closing the value it
//! replaces before it opens, so two holders never have it, and the replaced value is dropped.

use super::node_holding_the_stub_device_as_a_computed_value::{
    NodeHoldingTheStubDevice, THE_DEVICE_HOLDING_NODES_WIDTH,
    register_the_node_holding_the_stub_device_once, the_device_held_in,
};
use super::stub_exclusive_device::StubExclusiveDevice;
use super::stub_exclusive_device::StubExclusiveDeviceEvent::{Closed, Opened, Released};
use super::*;

/// How long an open may take to follow the value it was opened at.
const OPENED_WITHIN: Duration = Duration::from_secs(10);

const DEVICE_HOLDING_NODE_UUID: &str = "7d7d7d7d-7d7d-4d7d-8d7d-7d7d7d7d7d7d";

/// A project whose stream `main` holds one [`NodeHoldingTheStubDevice`] named `camera`, its
/// `width` 640.
fn a_project_whose_main_stream_holds_the_stub_device_as_a_computed_value()
-> ProjectWithGraphDocuments {
    register_the_node_holding_the_stub_device_once();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": DEVICE_HOLDING_NODE_UUID, "name": "camera",
              "type": NodeHoldingTheStubDevice::node_class_import_path().as_str(),
              "input_values": { THE_DEVICE_HOLDING_NODES_WIDTH: 640 } },
        ],
        "links": [],
    }))
}

async fn wait_until_the_device_of(
    stream_id: &str,
    counted: fn(&StubExclusiveDevice) -> usize,
    reaches: usize,
) {
    let deadline = std::time::Instant::now() + OPENED_WITHIN;
    while the_device_held_in(stream_id, |held| counted(held.device())) < reaches {
        assert!(
            std::time::Instant::now() < deadline,
            "the device's count reaches {reaches} within {OPENED_WITHIN:?}: {:?}",
            the_device_held_in(stream_id, |held| held.device().events())
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Mental-revert: a set that never rebuilds the computed value opens the device once; an engine
/// that keeps the replaced value never releases its lease; a node rebuilt by the set runs its
/// body again.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_opened_again_by_an_input_set_while_playing_closes_the_one_it_replaces_first() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_the_stub_device_as_a_computed_value();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    wait_until_the_device_of(&stream_id, StubExclusiveDevice::opens, 1).await;

    let answer = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "set_node_input_value", "stream": project.stream_asset_of("main"),
                  "node": DEVICE_HOLDING_NODE_UUID, "input": THE_DEVICE_HOLDING_NODES_WIDTH,
                  "value": 1280 },
            ] }),
        )
        .await
        .expect("the width is set");
    assert!(
        answer.get("nodes_rebuilt").is_none(),
        "no node is rebuilt: {answer}"
    );
    wait_until_the_device_of(&stream_id, StubExclusiveDevice::releases, 1).await;

    let (bodies_run, events) = the_device_held_in(&stream_id, |held| {
        (held.bodies_run(), held.device().events())
    });
    assert_eq!(bodies_run, 1, "the node's body ran once");
    assert_eq!(
        events,
        [
            Opened { holder: 0 },
            Closed { holder: 0 },
            Opened { holder: 1 },
            Released { holder: 0 }
        ],
        "the device opened at 640 closes before the one at 1280 opens, and its value is dropped"
    );
    let listing = the_listing_of(&engine.operations_on_the_loaded_streams(), &stream_id).await;
    assert_eq!(listing["state"], "running", "{listing}");
    assert_eq!(listing["condition"]["health"], "healthy", "{listing}");
    assert_the_graph_document_equals_the_running_graph(
        &project.the_graph_document_of("main"),
        &engine.the_live_graph_of(&stream_id),
    );
}
