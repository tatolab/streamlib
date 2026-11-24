// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node added while its stream plays, holding a value on a Float input, takes a link made
//! into that input in a later edit: the link is wired and the reader reads what it carries, as
//! for a node present when the stream was opened.

use std::time::Instant;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::iceoryx2::MessageGroupBoundary;
use streamlib::sdk::nodes::{ContinuousNode, ReactiveNode};
use streamlib::sdk::pin_value_packing::{PackedPinValueRef, pack_pin_value, unpack_pin_value};

use super::graph_input_retyped_through_transact_tests::transact_by_alice;
use super::*;

/// The UUID the graph document gives its `writer`.
const WRITER_NODE_UUID: &str = "7e7e7e7e-7e7e-4e7e-8e7e-7e7e7e7e7e7e";

/// What [`StreamToolsTestLevelWriter`] writes on its `level` at each interval.
const THE_LEVEL_WRITTEN: f64 = 7.5;

/// The value the reader holds on its unlinked `level` when it is added.
const THE_LEVEL_HELD_UNLINKED: f64 = 1.0;

/// How long the written level may take to reach the reader once the link is made.
const ARRIVES_WITHIN: Duration = Duration::from_secs(10);

/// Every Float a [`StreamToolsTestLevelReader`] has read off a link.
static LEVELS_READ: Mutex<Vec<f64>> = Mutex::new(Vec::new());

/// A native source writing [`THE_LEVEL_WRITTEN`] on its Float `level` every 5 ms.
#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    output("level", pin_type = Float)
)]
pub struct StreamToolsTestLevelWriter;

impl ContinuousNode for StreamToolsTestLevelWriter::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let packed = pack_pin_value(
            &PinType::Float { exact_size: None },
            &json!(THE_LEVEL_WRITTEN),
            0,
        )
        .expect("a Float packs");
        self.outputs.write_bytes_described_by(
            "level",
            &packed.payload,
            0,
            MessageGroupBoundary::StartsGroup,
            packed.payload_descriptor,
        )
    }
}

/// A native reader of Floats on its input `level`, which records each Float a link carried.
#[streamlib::sdk::node(
    execution = reactive,
    input("level", delivery_profile = "newest", pin_type = Float)
)]
pub struct StreamToolsTestLevelReader;

impl ReactiveNode for StreamToolsTestLevelReader::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("level")? {
            let level = unpack_pin_value(
                &PinType::Float { exact_size: None },
                PackedPinValueRef {
                    payload_descriptor: &message.payload_descriptor,
                    payload: message.payload.bytes(),
                },
            )
            .expect("a Float message unpacks");
            LEVELS_READ
                .lock()
                .push(level.as_f64().expect("a Float unpacks to a number"));
        }
        Ok(())
    }
}

/// A project whose stream `main` holds one [`StreamToolsTestLevelWriter`] `writer`.
fn a_project_whose_main_stream_holds_a_level_writer() -> ProjectWithGraphDocuments {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY.register::<StreamToolsTestLevelWriter::Node>();
        NODE_REGISTRY.register::<StreamToolsTestLevelReader::Node>();
    });
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{ "id": WRITER_NODE_UUID, "name": "writer",
                    "type": StreamToolsTestLevelWriter::node_class_import_path().as_str() }],
        "links": [],
    }))
}

/// A reader added to a playing stream holding 1.0 on `level`, then linked from the writer's
/// `level` in a second edit, has the link wired and reads 7.5.
///
/// Mental-revert: wiring the link skips opening a mailbox for an input that already reads its
/// held value, so the link errors with `Unknown input port: level` and no 7.5 arrives.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_input_of_a_node_added_while_playing_takes_a_link_made_later() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_level_writer();
    engine.playing(&project).await;
    let stream = project.stream_asset_of("main");
    let stream_id = project.stream_id_of("main");

    transact_by_alice(
        &engine,
        json!([{ "operation": "add_node", "stream": stream, "name": "reader",
                 "type": StreamToolsTestLevelReader::node_class_import_path().as_str(),
                 "pins": { "level": THE_LEVEL_HELD_UNLINKED } }]),
    )
    .await
    .expect("the reader is added while the stream plays");
    let live = engine.the_live_graph_of(&stream_id);
    let (writer, reader) = (
        the_node_named(&live, "writer")["id"].clone(),
        the_node_named(&live, "reader")["id"].clone(),
    );

    transact_by_alice(
        &engine,
        json!([{ "operation": "connect", "stream": stream,
                 "source": { "node": writer, "port": "level" },
                 "target": { "node": reader, "port": "level" } }]),
    )
    .await
    .expect("the link into the reader's level is made");

    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    while !LEVELS_READ.lock().contains(&THE_LEVEL_WRITTEN) {
        let links = engine.the_live_graph_of(&stream_id)["links"].clone();
        assert!(
            Instant::now() < gave_up_at,
            "the reader read no {THE_LEVEL_WRITTEN}; the links are {links}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let links = engine.the_live_graph_of(&stream_id)["links"].clone();
    let states: Vec<&Value> = links
        .as_array()
        .expect("the graph lists its links")
        .iter()
        .map(|link| &link["state"])
        .collect();
    assert_eq!(states, [&json!("wired")], "{links}");
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}
